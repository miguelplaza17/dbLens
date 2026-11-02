//! MySQL and MariaDB driver (mysql_async, pure Rust, rustls). Port of
//! `Sources/DBLensKit/MySQL.swift`: same SQL, same semantics.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use mysql_async::consts::{ColumnFlags, ColumnType};
use mysql_async::prelude::Queryable;
use mysql_async::{Column as MyColumn, Conn, Opts, OptsBuilder, Row, SslOpts, Value};
use tokio::sync::Mutex;

use crate::connector::Summary;
use crate::engine::Engine;
use crate::session::{
    keyset_direction, like_pattern, Activity, CellValue, Column, ColumnSort, DatabaseSession,
    DbError, DbResult, EditOutcome, Execution, KeyHint, QueryResult, Relation, RelationKind,
    RowKey, RowKeyKind, RowKeys, SchemaGraph, DEEP_OFFSET, ROW_LIMIT,
};
use crate::sql_script::{self, Dialect};
use crate::url::UrlParts;
use crate::wire::StreamSink;

const SCHEMES: [&str; 3] = ["mysql", "mariadb", "mysqlx"];
const SYSTEM_SCHEMAS: &str = "('mysql','information_schema','performance_schema','sys')";
/// Tables with more estimated rows than this are not counted exactly.
const COUNT_LIMIT: f64 = 500_000.0;
/// Bytes shown as 0x…, cut after this many, so the grid stays fast.
const HEX_LIMIT: usize = 64;

/// Host, database, user and TLS mode for the connection sheet.
pub fn summary(text: &str) -> Option<Summary> {
    let parts = UrlParts::new(text).filter(|p| SCHEMES.contains(&p.scheme.as_str()))?;
    let host = parts
        .first_host()
        .map(|h| h.name.clone())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "localhost".into());
    let tls = parts
        .query
        .iter()
        .find(|(k, _)| ["ssl-mode", "sslmode", "ssl_mode"].contains(&k.to_lowercase().as_str()))
        .map(|(_, v)| v.to_uppercase())
        .or_else(|| {
            parts
                .query
                .iter()
                .find(|(k, v)| {
                    ["ssl", "usessl"].contains(&k.to_lowercase().as_str())
                        && v.eq_ignore_ascii_case("true")
                })
                .map(|_| "REQUIRED".into())
        });
    let database = parts.path.split('/').next().unwrap_or("").to_string();
    Some(Summary {
        engine: Engine::Mysql,
        suggested_name: format!(
            "{} @ {host}",
            if database.is_empty() {
                "mysql"
            } else {
                &database
            }
        ),
        database,
        host: Some(host),
        port: parts.first_host().and_then(|h| h.port),
        user: parts.user.clone(),
        tls,
    })
}

// MARK: - Target

/// MySQL's own words for the TLS setting: DISABLED, PREFERRED, REQUIRED,
/// VERIFY_CA, VERIFY_IDENTITY. None means the URL says nothing (PREFERRED).
pub(crate) fn ssl_mode(query: &std::collections::BTreeMap<String, String>) -> Option<&'static str> {
    let q: std::collections::HashMap<String, &str> = query
        .iter()
        .map(|(k, v)| (k.to_lowercase(), v.as_str()))
        .collect();
    if let Some(mode) = q
        .get("ssl-mode")
        .or_else(|| q.get("sslmode"))
        .or_else(|| q.get("ssl_mode"))
    {
        return match mode.to_lowercase().replace('-', "_").as_str() {
            "disabled" | "disable" | "false" | "0" | "off" => Some("DISABLED"),
            "preferred" | "prefer" | "allow" => Some("PREFERRED"),
            "required" | "require" | "true" | "1" | "on" | "trust" | "no_verify" => {
                Some("REQUIRED")
            }
            "verify_ca" => Some("VERIFY_CA"),
            "verify_identity" | "verify_full" => Some("VERIFY_IDENTITY"),
            _ => None,
        };
    }
    let flag = q
        .get("ssl")
        .or_else(|| q.get("usessl"))
        .or_else(|| q.get("requiressl"))?
        .to_lowercase();
    if ["false", "0", "off", "no"].contains(&flag.as_str()) {
        return Some("DISABLED");
    }
    if !["true", "1", "on", "yes", "require", "required"].contains(&flag.as_str()) {
        return None;
    }
    if q.get("verifyservercertificate")
        .map(|v| v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
    {
        Some("VERIFY_CA")
    } else {
        Some("REQUIRED")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tls {
    Off,
    /// Encrypted, certificate not checked.
    Required,
    VerifyCa,
    VerifyIdentity,
}

#[derive(Debug, Clone)]
struct Target {
    host: String,
    port: u16,
    user: Option<String>,
    password: Option<String>,
    database: Option<String>,
    socket: Option<String>,
    ssl_mode: Option<&'static str>,
    /// The URL's host when connecting through a tunnel, for the certificate check.
    tls_name: Option<String>,
}

impl Target {
    fn new(url: &str, local_port: Option<u16>) -> DbResult<Self> {
        let parts = UrlParts::new(url)
            .filter(|p| SCHEMES.contains(&p.scheme.as_str()))
            .ok_or_else(|| DbError::msg("This doesn't look like a MySQL URL."))?;
        let url_host = parts
            .first_host()
            .map(|h| h.name.clone())
            .filter(|h| !h.is_empty())
            .unwrap_or_else(|| "localhost".into());
        let url_port = parts.first_host().and_then(|h| h.port).unwrap_or(3306);
        let database = parts
            .path
            .split('/')
            .next()
            .filter(|d| !d.is_empty())
            .map(String::from);
        let socket = parts
            .query
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("socket"))
            .map(|(_, v)| v.clone())
            .filter(|s| !s.is_empty());
        let (host, port, socket, tls_name) = match local_port {
            Some(p) => ("127.0.0.1".to_string(), p, None, Some(url_host)),
            None => (url_host, url_port, socket, None),
        };
        Ok(Self {
            host,
            port,
            user: parts.user.clone().filter(|u| !u.is_empty()),
            password: parts.password.clone(),
            database,
            socket,
            ssl_mode: ssl_mode(&parts.query),
            tls_name,
        })
    }

    fn opts(&self, tls: Tls) -> Opts {
        let ssl = match tls {
            Tls::Off => None,
            Tls::Required => Some(
                SslOpts::default()
                    .with_danger_accept_invalid_certs(true)
                    .with_danger_skip_domain_validation(true),
            ),
            Tls::VerifyCa => Some(SslOpts::default().with_danger_skip_domain_validation(true)),
            Tls::VerifyIdentity => {
                Some(SslOpts::default().with_danger_tls_hostname_override(self.tls_name.clone()))
            }
        };
        OptsBuilder::default()
            .ip_or_hostname(self.host.clone())
            .tcp_port(self.port)
            .user(self.user.clone())
            .pass(self.password.clone())
            .db_name(self.database.clone())
            .socket(self.socket.clone())
            .ssl_opts(ssl)
            // Like the C client's CLIENT_FOUND_ROWS: affected = matched rows.
            .client_found_rows(true)
            .prefer_socket(false)
            .into()
    }

    /// Opens a connection and returns it with the options that worked.
    /// PREFERRED (the default) tries TLS first and falls back to plain TCP.
    async fn open(&self) -> DbResult<(Conn, Opts)> {
        let attempts: Vec<Tls> = match self.ssl_mode {
            Some("DISABLED") => vec![Tls::Off],
            Some("REQUIRED") => vec![Tls::Required],
            Some("VERIFY_CA") => vec![Tls::VerifyCa],
            Some("VERIFY_IDENTITY") => vec![Tls::VerifyIdentity],
            _ => vec![Tls::Required, Tls::Off],
        };
        let mut last = None;
        for tls in attempts {
            let opts = self.opts(tls);
            let connecting = Conn::new(opts.clone());
            match tokio::time::timeout(Duration::from_secs(10), connecting).await {
                Ok(Ok(conn)) => return Ok((conn, opts)),
                Ok(Err(error)) => last = Some(to_error(&error)),
                Err(_) => last = Some(DbError::msg("Connection timed out.")),
            }
        }
        Err(last.unwrap_or_else(|| DbError::msg("Couldn't connect.")))
    }
}

fn to_error(error: &mysql_async::Error) -> DbError {
    match error {
        mysql_async::Error::Server(server) => DbError::msg(server.message.trim().to_string()),
        other => {
            let text = other.to_string();
            DbError::msg(if text.trim().is_empty() {
                "The server closed the connection.".to_string()
            } else {
                text
            })
        }
    }
}

/// Opens a session. Through an SSH tunnel, `local_port` replaces the URL's host and port.
pub async fn connect(url: &str, local_port: Option<u16>) -> DbResult<Box<dyn DatabaseSession>> {
    let target = Target::new(url, local_port)?;
    let (mut conn, opts) = target.open().await?;
    let info: Option<String> = conn
        .query_first("select version()")
        .await
        .map_err(|e| to_error(&e))?;
    let state = Arc::new(Shared {
        activity: Activity::default(),
        editing: AtomicBool::new(false),
        in_transaction: AtomicBool::new(false),
        autocommit_off: AtomicBool::new(false),
        scope_changed: AtomicBool::new(false),
        id: conn.id(),
        opts,
    });
    Ok(Box::new(MySqlSession {
        conn: Mutex::new(Some(conn)),
        state,
        database: target.database,
        server_version: version(&info.unwrap_or_default()),
    }))
}

/// "8.4.3", or "11.4.2-MariaDB" without the distribution suffix.
pub(crate) fn version(info: &str) -> String {
    match info.find("-MariaDB") {
        Some(at) => info[..at + "-MariaDB".len()].to_string(),
        None => info.to_string(),
    }
}

// MARK: - Session

/// What `cancel` needs without the connection's lock.
struct Shared {
    /// The running cancellable statement; KILL QUERY goes out only while it runs.
    activity: Activity,
    /// An edit is running: its statements are not cancellable, so a cancel never
    /// lands between its own START TRANSACTION and COMMIT.
    editing: AtomicBool,
    /// The user's transaction is open, as of the last SQL run.
    in_transaction: AtomicBool,
    /// The user turned autocommit off: any statement may open a transaction
    /// then, so every run asks the server again.
    autocommit_off: AtomicBool,
    /// `USE` in the panel moved the connection off the URL's database.
    scope_changed: AtomicBool,
    id: u32,
    opts: Opts,
}

pub struct MySqlSession {
    conn: Mutex<Option<Conn>>,
    state: Arc<Shared>,
    database: Option<String>,
    server_version: String,
}

fn closed() -> DbError {
    DbError::msg("The connection is closed.")
}

/// Statements MySQL commits by itself (implicit commit): a transaction can't undo them.
pub(crate) fn commits_by_itself(statement: &str) -> bool {
    let verb = sql_script::verb(statement, 1);
    if verb == "CREATE" && sql_script::verb(statement, 2) == "CREATE TEMPORARY" {
        return false;
    }
    [
        "CREATE",
        "ALTER",
        "DROP",
        "RENAME",
        "TRUNCATE",
        "GRANT",
        "REVOKE",
        "LOCK",
        "UNLOCK",
        "INSTALL",
        "UNINSTALL",
        "ANALYZE",
        "OPTIMIZE",
        "REPAIR",
        "CHECK",
        "FLUSH",
        "RESET",
        "CALL",
        "LOAD",
    ]
    .contains(&verb.as_str())
}

/// The first word of each statement, uppercased, skipping comments and quoted
/// text: "insert …; select …" -> ["INSERT", "SELECT"].
pub(crate) fn statement_verbs(sql: &str) -> Vec<String> {
    let bytes = sql.as_bytes();
    let mut verbs: Vec<String> = Vec::new();
    let mut verb: Vec<u8> = Vec::new();
    let (mut started, mut collecting) = (false, false);
    let mut i = 0;
    let skip_to = |i: &mut usize, end: &[u8]| {
        while *i < bytes.len() && !bytes[*i..].starts_with(end) {
            *i += 1;
        }
        *i = bytes.len().min(*i + end.len());
    };
    while i < bytes.len() {
        let byte = bytes[i];
        let next = bytes.get(i + 1).copied().unwrap_or(0);
        match byte {
            b'-' if next == b'-'
                && (i + 2 >= bytes.len() || [9, 10, 13, 32].contains(&bytes[i + 2])) =>
            {
                skip_to(&mut i, b"\n");
                collecting = false;
            }
            b'#' => {
                skip_to(&mut i, b"\n");
                collecting = false;
            }
            b'/' if next == b'*' => {
                i += 2;
                skip_to(&mut i, b"*/");
                collecting = false;
            }
            b'\'' | b'"' | b'`' => {
                started = true;
                collecting = false;
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' && byte != b'`' {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == byte {
                        if bytes.get(i + 1) == Some(&byte) {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
                i += 1;
            }
            b';' => {
                if started {
                    verbs.push(String::from_utf8_lossy(&verb).to_uppercase());
                }
                started = false;
                collecting = false;
                verb.clear();
                i += 1;
            }
            9 | 10 | 13 | 32 => {
                collecting = false;
                i += 1;
            }
            _ => {
                if !started {
                    started = true;
                    collecting = true;
                }
                if collecting && byte.is_ascii_alphabetic() {
                    verb.push(byte);
                } else {
                    collecting = false;
                }
                i += 1;
            }
        }
    }
    if started {
        verbs.push(String::from_utf8_lossy(&verb).to_uppercase());
    }
    verbs
}

/// Bytes as 0x…, cut after `HEX_LIMIT`.
pub(crate) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789ABCDEF";
    let mut text = String::with_capacity(2 + bytes.len().min(HEX_LIMIT) * 2 + 3);
    text.push_str("0x");
    for byte in bytes.iter().take(HEX_LIMIT) {
        text.push(DIGITS[(byte >> 4) as usize] as char);
        text.push(DIGITS[(byte & 0xF) as usize] as char);
    }
    if bytes.len() > HEX_LIMIT {
        text.push('…');
    }
    text
}

/// A quoted string literal for a connection with (or without) backslash
/// escapes; NULL for None.
pub(crate) fn literal(value: Option<&str>, no_backslash_escapes: bool) -> String {
    let Some(value) = value else {
        return "NULL".into();
    };
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for c in value.chars() {
        match c {
            '\'' => out.push_str("''"),
            _ if no_backslash_escapes => out.push(c),
            '\\' => out.push_str("\\\\"),
            '\0' => out.push_str("\\0"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '"' => out.push_str("\\\""),
            '\u{1a}' => out.push_str("\\Z"),
            _ => out.push(c),
        }
    }
    out.push('\'');
    out
}

fn quote_name(identifier: &str) -> String {
    format!("`{}`", identifier.replace('`', "``"))
}

/// How a key column is read in `page` and found again in an edit. What the
/// grid shows is not always exact: bytes are cut and floats are rounded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyForm {
    /// Compared as a string literal.
    Text,
    /// BINARY, VARBINARY, BLOB: every byte as hex, compared as `x'…'`.
    Hex,
    /// BIT: its number.
    Number,
    /// FLOAT, DOUBLE: the server's own text for it, compared as text.
    FloatText,
}

/// The form for a column from its `COLUMN_TYPE` ("binary(16)", "float unsigned").
fn key_form(column_type: &str) -> KeyForm {
    let lower = column_type.trim().to_lowercase();
    let base = lower
        .split(|c: char| c == '(' || c.is_whitespace())
        .next()
        .unwrap_or("");
    match base {
        "binary" | "varbinary" | "tinyblob" | "blob" | "mediumblob" | "longblob" => KeyForm::Hex,
        "bit" => KeyForm::Number,
        "float" | "double" | "real" => KeyForm::FloatText,
        _ => KeyForm::Text,
    }
}

/// The hidden select expression that reads a key column exactly.
fn key_select(column: &str, form: KeyForm) -> String {
    let name = format!("t.{}", quote_name(column));
    match form {
        KeyForm::Text => name,
        KeyForm::Hex => format!("hex({name})"),
        KeyForm::Number => format!("{name}+0"),
        KeyForm::FloatText => format!("cast({name} as char)"),
    }
}

/// A key value (as `key_select` read it) as a literal to compare the column with.
fn key_literal(form: KeyForm, value: &str, backslash_off: bool) -> Option<String> {
    match form {
        KeyForm::Text => Some(literal(Some(value), backslash_off)),
        KeyForm::Hex => {
            let digits = value
                .strip_prefix("0x")
                .or_else(|| value.strip_prefix("0X"))
                .unwrap_or(value);
            (digits.len() % 2 == 0 && digits.chars().all(|c| c.is_ascii_hexdigit()))
                .then(|| format!("x'{digits}'"))
        }
        KeyForm::Number => (!value.is_empty() && value.chars().all(|c| c.is_ascii_digit()))
            .then(|| value.to_string()),
        KeyForm::FloatText => None,
    }
}

/// `col = value` for one key column, as sent and as logged.
fn key_test(
    column: &str,
    form: KeyForm,
    value: &str,
    backslash_off: bool,
) -> DbResult<(String, String)> {
    let name = quote_name(column);
    let bad = || DbError::msg("The row key is not valid. Reload the table and try again.");
    Ok(match form {
        KeyForm::Text => (
            format!("{name} = {}", literal(Some(value), backslash_off)),
            format!("{name} = ?"),
        ),
        KeyForm::Hex => {
            let digits = value
                .strip_prefix("0x")
                .or_else(|| value.strip_prefix("0X"))
                .unwrap_or(value);
            if digits.len() % 2 != 0 || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(bad());
            }
            (format!("{name} = x'{digits}'"), format!("{name} = ?"))
        }
        KeyForm::Number => {
            if value.is_empty() || !value.chars().all(|c| c.is_ascii_digit()) {
                return Err(bad());
            }
            (format!("{name} = {value}"), format!("{name} = ?"))
        }
        KeyForm::FloatText => (
            format!(
                "cast({name} as char) = {}",
                literal(Some(value), backslash_off)
            ),
            format!("cast({name} as char) = ?"),
        ),
    })
}

/// Bytes, not text: BLOB, BINARY, VARBINARY, BIT and geometry in the binary charset.
fn is_binary(column: &MyColumn) -> bool {
    if column.character_set() != 63 {
        return false;
    }
    use ColumnType::*;
    matches!(
        column.column_type(),
        MYSQL_TYPE_TINY_BLOB
            | MYSQL_TYPE_MEDIUM_BLOB
            | MYSQL_TYPE_LONG_BLOB
            | MYSQL_TYPE_BLOB
            | MYSQL_TYPE_STRING
            | MYSQL_TYPE_VAR_STRING
            | MYSQL_TYPE_VARCHAR
            | MYSQL_TYPE_BIT
            | MYSQL_TYPE_GEOMETRY
    )
}

/// The type of a result column: "int", "varchar", "datetime", "json"…
fn type_name(column: &MyColumn) -> &'static str {
    use ColumnType::*;
    let flags = column.flags();
    if flags.contains(ColumnFlags::ENUM_FLAG) {
        return "enum";
    }
    if flags.contains(ColumnFlags::SET_FLAG) {
        return "set";
    }
    let binary = column.character_set() == 63;
    match column.column_type() {
        MYSQL_TYPE_TINY => "tinyint",
        MYSQL_TYPE_SHORT => "smallint",
        MYSQL_TYPE_INT24 => "mediumint",
        MYSQL_TYPE_LONG => "int",
        MYSQL_TYPE_LONGLONG => "bigint",
        MYSQL_TYPE_FLOAT => "float",
        MYSQL_TYPE_DOUBLE => "double",
        MYSQL_TYPE_DECIMAL | MYSQL_TYPE_NEWDECIMAL => "decimal",
        MYSQL_TYPE_NULL => "null",
        MYSQL_TYPE_TIMESTAMP | MYSQL_TYPE_TIMESTAMP2 => "timestamp",
        MYSQL_TYPE_DATE | MYSQL_TYPE_NEWDATE => "date",
        MYSQL_TYPE_TIME | MYSQL_TYPE_TIME2 => "time",
        MYSQL_TYPE_DATETIME | MYSQL_TYPE_DATETIME2 => "datetime",
        MYSQL_TYPE_YEAR => "year",
        MYSQL_TYPE_VARCHAR | MYSQL_TYPE_VAR_STRING => {
            if binary {
                "varbinary"
            } else {
                "varchar"
            }
        }
        MYSQL_TYPE_STRING => {
            if binary {
                "binary"
            } else {
                "char"
            }
        }
        MYSQL_TYPE_BIT => "bit",
        MYSQL_TYPE_JSON => "json",
        MYSQL_TYPE_ENUM => "enum",
        MYSQL_TYPE_SET => "set",
        MYSQL_TYPE_TINY_BLOB | MYSQL_TYPE_MEDIUM_BLOB | MYSQL_TYPE_LONG_BLOB | MYSQL_TYPE_BLOB => {
            if binary {
                "blob"
            } else {
                "text"
            }
        }
        MYSQL_TYPE_GEOMETRY => "geometry",
        MYSQL_TYPE_VECTOR => "vector",
        _ => "",
    }
}

fn cell(value: &Value, binary: bool) -> Option<String> {
    match value {
        Value::NULL => None,
        Value::Bytes(bytes) => Some(if binary {
            hex(bytes)
        } else {
            String::from_utf8_lossy(bytes).into_owned()
        }),
        other => Some(other.as_sql(true).trim_matches('\'').to_string()),
    }
}

fn status(verb: &str, affected: u64) -> QueryResult {
    let command = if ["INSERT", "UPDATE", "DELETE", "REPLACE"].contains(&verb) {
        format!("{verb} {affected}")
    } else if verb.is_empty() {
        "OK".to_string()
    } else {
        verb.to_string()
    };
    QueryResult::command(command, affected)
}

/// Whether the connection treats backslash as a plain character.
async fn no_backslash_escapes(conn: &mut Conn) -> DbResult<bool> {
    let mode: Option<String> = conn
        .query_first("select @@SESSION.sql_mode")
        .await
        .map_err(|e| to_error(&e))?;
    Ok(mode
        .unwrap_or_default()
        .to_uppercase()
        .contains("NO_BACKSLASH_ESCAPES"))
}

impl MySqlSession {
    async fn guard(&self) -> DbResult<tokio::sync::MappedMutexGuard<'_, Conn>> {
        let guard = self.conn.lock().await;
        tokio::sync::MutexGuard::try_map(guard, |c| c.as_mut()).map_err(|_| closed())
    }

    /// Runs one or more statements, the way the mysql client does. A canceled
    /// run is an error, also when the server ended it quietly.
    async fn run_all(&self, conn: &mut Conn, sql: &str) -> DbResult<Vec<QueryResult>> {
        if self.state.editing.load(Ordering::SeqCst) {
            return read_all(conn, sql).await;
        }
        let id = self.state.activity.start();
        let outcome = read_all(conn, sql).await;
        if self.state.activity.finish(id) {
            return Err(DbError::Cancelled);
        }
        outcome
    }

    /// A page, starting from `hint` (keyset pagination) or a key found by a
    /// key-only query for deep offsets, when the order is the key's.
    #[allow(clippy::too_many_arguments)]
    async fn page_in(
        &self,
        conn: &mut Conn,
        relation: &Relation,
        limit: u32,
        offset: u64,
        search: &str,
        sort: Option<&ColumnSort>,
        hint: Option<&KeyHint>,
    ) -> DbResult<QueryResult> {
        let primary_key = self.primary_key(conn, relation).await?;
        // MySQL sorts NULL first going up; "x is null" puts it last either way.
        let mut terms: Vec<String> = Vec::new();
        if let Some(sort) = sort {
            let name = quote_name(&sort.column);
            terms.push(format!("{name} is null"));
            terms.push(format!("{name} {}", sort.direction()));
        }
        terms.extend(primary_key.iter().map(|c| quote_name(c)));
        let types = self.column_types(conn, relation).await?;
        if primary_key.is_empty() {
            // No key (a view, a table without one): every column breaks ties, so
            // blocks fetched one by one with OFFSET neither overlap nor skip rows.
            terms.extend(types.iter().map(|(name, _)| quote_name(name)));
        }
        let order = if terms.is_empty() {
            String::new()
        } else {
            format!(" order by {}", terms.join(", "))
        };
        let off = if search.is_empty() && hint.is_none() && offset < DEEP_OFFSET {
            false
        } else {
            no_backslash_escapes(conn).await?
        };
        let mut conditions: Vec<String> = Vec::new();
        if !search.is_empty() {
            let names: Vec<String> = types.iter().map(|t| t.0.clone()).collect();
            let filter = Self::filter(search, &names, off);
            if let Some(rest) = filter.strip_prefix(" where ") {
                conditions.push(rest.to_string());
            }
        }
        let forms: Vec<KeyForm> = primary_key
            .iter()
            .map(|column| {
                types
                    .iter()
                    .find(|(n, _)| n == column)
                    .map(|(_, t)| key_form(t))
                    .unwrap_or(KeyForm::Text)
            })
            .collect();
        let skip = self
            .keyset(
                conn,
                relation,
                &primary_key,
                &forms,
                search,
                sort,
                &order,
                offset,
                hint,
                off,
                &mut conditions,
            )
            .await?;
        let where_ = if conditions.is_empty() {
            String::new()
        } else {
            format!(" where {}", conditions.join(" and "))
        };
        // The key columns come first, read exactly (bytes as full hex, floats as
        // the server's text), and are taken off before the rows are shown.
        let hidden: Vec<String> = primary_key
            .iter()
            .enumerate()
            .map(|(i, column)| {
                let form = types
                    .iter()
                    .find(|(n, _)| n == column)
                    .map(|(_, t)| key_form(t))
                    .unwrap_or(KeyForm::Text);
                format!("{} as `dblens_key_{i}`, ", key_select(column, form))
            })
            .collect();
        let sql = format!(
            "select {}t.* from {} as t{where_}{order} limit {limit} offset {skip}",
            hidden.concat(),
            self.qualified(relation)
        );
        let mut result = self.single(conn, &sql).await?;
        let n = hidden.len();
        if n > 0 && result.columns.len() >= n {
            let values = result
                .rows
                .iter_mut()
                .map(|row| row.drain(..n).collect())
                .collect();
            result.columns.drain(..n);
            result.keys = Some(RowKeys {
                columns: primary_key,
                kind: RowKeyKind::PrimaryKey,
                values,
            });
        }
        for column in &mut result.columns {
            if let Some((_, t)) = types.iter().find(|(n, _)| n == &column.name) {
                column.type_name = t.clone();
            }
        }
        Ok(result)
    }

    /// Adds `key > hint` (or `>=` a key found by a key-only seek) to
    /// `conditions` and returns the rows still to skip.
    #[allow(clippy::too_many_arguments)]
    async fn keyset(
        &self,
        conn: &mut Conn,
        relation: &Relation,
        primary_key: &[String],
        forms: &[KeyForm],
        search: &str,
        sort: Option<&ColumnSort>,
        order: &str,
        offset: u64,
        hint: Option<&KeyHint>,
        off: bool,
        conditions: &mut Vec<String>,
    ) -> DbResult<u64> {
        let Some(ascending) = keyset_direction(primary_key, sort) else {
            return Ok(offset);
        };
        if forms.contains(&KeyForm::FloatText) {
            return Ok(offset);
        }
        let names: Vec<String> = primary_key
            .iter()
            .map(|k| format!("t.{}", quote_name(k)))
            .collect();
        let tuple = if names.len() == 1 {
            names[0].clone()
        } else {
            format!("({})", names.join(", "))
        };
        let literals = |values: &[Option<String>]| -> Option<String> {
            if values.len() != forms.len() {
                return None;
            }
            let parts: Option<Vec<String>> = values
                .iter()
                .zip(forms)
                .map(|(v, form)| key_literal(*form, v.as_deref()?, off))
                .collect();
            let parts = parts?;
            Some(if parts.len() == 1 {
                parts[0].clone()
            } else {
                format!("({})", parts.join(", "))
            })
        };
        let op = |inclusive: bool| match (ascending, inclusive) {
            (true, false) => ">",
            (true, true) => ">=",
            (false, false) => "<",
            (false, true) => "<=",
        };
        let mut skip = offset;
        let mut bound: Option<(String, bool)> = None;
        if let Some(hint) = hint.filter(|h| h.offset < offset) {
            if let Some(value) = literals(&hint.values) {
                bound = Some((value, false));
                skip = offset - hint.offset - 1;
            }
        }
        if skip >= DEEP_OFFSET && search.is_empty() {
            let seek = match &bound {
                Some((value, _)) => format!(" where {tuple} {} {value}", op(false)),
                None => String::new(),
            };
            let exprs: Vec<String> = primary_key
                .iter()
                .zip(forms)
                .map(|(k, f)| key_select(k, *f))
                .collect();
            let found = self
                .single(
                    conn,
                    &format!(
                        "select {} from {} as t{seek}{order} limit 1 offset {skip}",
                        exprs.join(", "),
                        self.qualified(relation)
                    ),
                )
                .await?;
            if let Some(value) = found.rows.first().and_then(|row| literals(row)) {
                bound = Some((value, true));
                skip = 0;
            }
        }
        if let Some((value, inclusive)) = bound {
            conditions.push(format!("{tuple} {} {value}", op(inclusive)));
        }
        Ok(skip)
    }

    /// Remembers whether the user's transaction is open after a run, asking
    /// the server only when the run could have changed it: a statement that
    /// opens one or sets autocommit, an open one, or autocommit off (then an
    /// INSERT or SELECT opens one by itself).
    async fn note_transaction(&self, conn: &mut Conn, sql: &str) {
        let changes = statement_verbs(sql)
            .iter()
            .any(|v| matches!(v.as_str(), "BEGIN" | "START" | "SET" | "XA"));
        let state = &self.state;
        if changes
            || state.in_transaction.load(Ordering::SeqCst)
            || state.autocommit_off.load(Ordering::SeqCst)
        {
            let now: Result<Option<(Option<i64>, Option<i64>)>, _> = conn
                .query_first("select @@in_transaction, @@autocommit")
                .await;
            let (open, autocommit) = match now {
                Ok(Some((open, autocommit))) => {
                    (open.unwrap_or(0) != 0, autocommit.unwrap_or(1) != 0)
                }
                // Unknown: keep asking after every run.
                _ => (false, false),
            };
            state.in_transaction.store(open, Ordering::SeqCst);
            state.autocommit_off.store(!autocommit, Ordering::SeqCst);
        }
    }

    /// Remembers whether `USE` moved the connection off the URL's database,
    /// asking the server only when the run had a USE (or it had moved).
    async fn note_scope(&self, conn: &mut Conn, sql: &str) {
        let uses = statement_verbs(sql).iter().any(|v| v == "USE");
        if uses || self.state.scope_changed.load(Ordering::SeqCst) {
            let now: Result<Option<Option<String>>, _> =
                conn.query_first("select database()").await;
            if let Ok(now) = now {
                let moved = now.flatten() != self.database;
                self.state.scope_changed.store(moved, Ordering::SeqCst);
            }
        }
    }

    /// Whether the user opened a transaction in the SQL panel (`@@in_transaction`,
    /// MySQL 5.7 and MariaDB 10.3 and later).
    async fn user_transaction(&self, conn: &mut Conn) -> DbResult<bool> {
        let open: Option<Option<i64>> = conn
            .query_first("select @@in_transaction")
            .await
            .map_err(|e| to_error(&e))?;
        Ok(open.flatten().unwrap_or(0) != 0)
    }

    /// Refuses to start dbLens's own transaction inside the user's: START
    /// TRANSACTION would commit the user's pending work by itself.
    async fn require_no_transaction(&self, conn: &mut Conn) -> DbResult<()> {
        if self.user_transaction(conn).await? {
            return Err(DbError::msg(
                "A transaction is open in the SQL panel. Commit or roll it back first.",
            ));
        }
        Ok(())
    }

    /// How each key column is read and found again.
    async fn key_forms(
        &self,
        conn: &mut Conn,
        relation: &Relation,
        columns: &[String],
    ) -> DbResult<Vec<KeyForm>> {
        let types = self.column_types(conn, relation).await?;
        Ok(columns
            .iter()
            .map(|c| {
                types
                    .iter()
                    .find(|(n, _)| n == c)
                    .map(|(_, t)| key_form(t))
                    .unwrap_or(KeyForm::Text)
            })
            .collect())
    }

    async fn single(&self, conn: &mut Conn, sql: &str) -> DbResult<QueryResult> {
        let mut results = self.run_all(conn, sql).await?;
        let at = results
            .iter()
            .rposition(QueryResult::has_rows)
            .or_else(|| results.len().checked_sub(1));
        match at {
            Some(at) => Ok(results.swap_remove(at)),
            None => Err(DbError::msg("No result.")),
        }
    }

    async fn silent(&self, conn: &mut Conn, sql: &str) -> DbResult<()> {
        self.run_all(conn, sql).await.map(|_| ())
    }

    async fn primary_key(&self, conn: &mut Conn, relation: &Relation) -> DbResult<Vec<String>> {
        if relation.kind != RelationKind::Table {
            return Ok(vec![]);
        }
        let sql = format!(
            "select COLUMN_NAME from information_schema.KEY_COLUMN_USAGE \
             where TABLE_SCHEMA = {s} and TABLE_NAME = {n} and CONSTRAINT_NAME = 'PRIMARY' order by ORDINAL_POSITION",
            s = literal(Some(&relation.schema), false),
            n = literal(Some(&relation.name), false)
        );
        Ok(self
            .single(conn, &sql)
            .await?
            .rows
            .into_iter()
            .filter_map(|r| r.into_iter().next().flatten())
            .collect())
    }

    /// Column name and type, in table order.
    async fn column_types(
        &self,
        conn: &mut Conn,
        relation: &Relation,
    ) -> DbResult<Vec<(String, String)>> {
        let sql = format!(
            "select COLUMN_NAME, COLUMN_TYPE from information_schema.COLUMNS \
             where TABLE_SCHEMA = {s} and TABLE_NAME = {n} order by ORDINAL_POSITION",
            s = literal(Some(&relation.schema), false),
            n = literal(Some(&relation.name), false)
        );
        Ok(self
            .single(conn, &sql)
            .await?
            .rows
            .into_iter()
            .map(|r| {
                (
                    r.first().cloned().flatten().unwrap_or_default(),
                    r.get(1).cloned().flatten().unwrap_or_default(),
                )
            })
            .collect())
    }

    /// Every column joined into one text (NULLs skipped) and matched with LIKE.
    fn filter(search: &str, columns: &[String], backslash_off: bool) -> String {
        if search.is_empty() || columns.is_empty() {
            return String::new();
        }
        let names = columns
            .iter()
            .map(|c| quote_name(c))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            " where concat_ws(' ', {names}) like {}",
            literal(Some(&like_pattern(search)), backslash_off)
        )
    }

    /// Runs one edit. An edit meant for exactly one row is rolled back if it
    /// would touch more, and refused if it touches none.
    async fn edit(
        &self,
        conn: &mut Conn,
        sent: &str,
        logged: String,
        expect_one: bool,
    ) -> DbResult<EditOutcome> {
        // Inside the user's open transaction the edit becomes part of it, behind a
        // savepoint (START TRANSACTION would commit the user's work): a refused
        // edit is undone alone, and the commit stays the user's.
        let nested = self.user_transaction(conn).await?;
        self.state.editing.store(true, Ordering::SeqCst);
        let outcome = self
            .edit_inner(conn, sent, logged, expect_one, nested)
            .await;
        self.state.editing.store(false, Ordering::SeqCst);
        outcome
    }

    async fn edit_inner(
        &self,
        conn: &mut Conn,
        sent: &str,
        logged: String,
        expect_one: bool,
        nested: bool,
    ) -> DbResult<EditOutcome> {
        let guarded = nested || expect_one;
        if nested {
            self.silent(conn, "savepoint dblens_edit").await?;
        } else if expect_one {
            self.silent(conn, "start transaction").await?;
        }
        let outcome = async {
            let result = self.single(conn, sent).await?;
            let affected = result.total_rows;
            if expect_one {
                if affected > 1 {
                    return Err(DbError::msg(format!("The change would have affected {affected} rows and was rolled back.")));
                }
                if affected != 1 {
                    return Err(DbError::msg("The row changed or was deleted since it was loaded. Reload the table and try again."));
                }
            }
            if guarded {
                self.silent(conn, if nested { "release savepoint dblens_edit" } else { "commit" }).await?;
            }
            Ok(EditOutcome { sql: logged, command: result.command, affected })
        }
        .await;
        if outcome.is_err() && guarded {
            let undo = if nested {
                "rollback to savepoint dblens_edit; release savepoint dblens_edit"
            } else {
                "rollback"
            };
            let _ = self.silent(conn, undo).await;
        }
        outcome
    }

    /// `where` for one row: the key columns, NULLs as `is null`. (sent, logged).
    fn where_clause(
        key: &RowKey,
        forms: &[KeyForm],
        backslash_off: bool,
    ) -> DbResult<(String, String)> {
        let (mut sent, mut logged) = (Vec::new(), Vec::new());
        for (i, column) in key.columns.iter().enumerate() {
            let name = quote_name(column);
            match key.values.get(i).cloned().flatten() {
                Some(v) => {
                    let form = forms.get(i).copied().unwrap_or(KeyForm::Text);
                    let (s, l) = key_test(column, form, &v, backslash_off)?;
                    sent.push(s);
                    logged.push(l);
                }
                None => {
                    sent.push(format!("{name} is null"));
                    logged.push(format!("{name} is null"));
                }
            }
        }
        // Non-null comparisons first, then the NULL tests, like the Swift app.
        let order = |parts: Vec<String>| {
            let (nulls, rest): (Vec<_>, Vec<_>) =
                parts.into_iter().partition(|p| p.ends_with(" is null"));
            rest.into_iter()
                .chain(nulls)
                .collect::<Vec<_>>()
                .join(" and ")
        };
        Ok((order(sent), order(logged)))
    }

    /// The script in a transaction, committed or always rolled back. Only data
    /// changes: DDL commits by itself in MySQL and would stay.
    async fn in_transaction(&self, sql: &str, commit: bool) -> DbResult<Execution> {
        let statements = sql_script::checked(sql, Dialect::Mysql)?;
        if let Some(index) = statements.iter().position(|s| commits_by_itself(s)) {
            let verb = sql_script::verb(&statements[index], 1);
            let n = index + 1;
            return Err(if commit {
                DbError::NotAtomic(format!("Statement {n} ({verb}) commits by itself in MySQL, so the script can't run all or nothing."))
            } else {
                DbError::msg(format!(
                    "Statement {n} ({verb}) commits by itself in MySQL, so it can't be tried and undone. Test data changes only, or try the DDL on a copy."
                ))
            });
        }
        let start = Instant::now();
        let mut conn = self.guard().await?;
        self.require_no_transaction(&mut conn).await?;
        self.silent(&mut conn, "start transaction").await?;
        let outcome = async {
            let mut results = Vec::with_capacity(statements.len());
            for (index, statement) in statements.iter().enumerate() {
                let run = self
                    .run_all(&mut conn, statement)
                    .await
                    .and_then(|mut r| r.pop().ok_or_else(|| DbError::msg("No result.")));
                match run {
                    Ok(result) => results.push(result),
                    Err(DbError::Cancelled) => return Err(DbError::Cancelled),
                    Err(error) => {
                        return Err(sql_script::failed(
                            index,
                            statements.len(),
                            statement,
                            &error,
                        ))
                    }
                }
            }
            self.silent(&mut conn, if commit { "commit" } else { "rollback" })
                .await?;
            Ok(results)
        }
        .await;
        match outcome {
            Ok(results) => Ok(Execution::new(results, start.elapsed())),
            Err(error) => {
                let _ = self.silent(&mut conn, "rollback").await;
                Err(error)
            }
        }
    }
}

/// Sends KILL QUERY for the session's connection from a second, short one.
/// With `op`, only while that operation still runs (checked once connected).
async fn kill_query(state: &Shared, op: Option<u64>) {
    let connecting = Conn::new(state.opts.clone());
    if let Ok(Ok(mut conn)) = tokio::time::timeout(Duration::from_secs(10), connecting).await {
        let still = match op {
            Some(op) => state.activity.is_running(op),
            None => true,
        };
        if still {
            let _ = conn.query_drop(format!("KILL QUERY {}", state.id)).await;
        }
        let _ = conn.disconnect().await;
    }
}

/// Reads every result set of `sql` into `sink` as the rows arrive. When the
/// sink closes mid-result, the server stops the query (KILL QUERY) and the
/// rest is read off here, so the connection is free for the next command
/// right away instead of draining the whole result first.
async fn stream_all(
    conn: &mut Conn,
    state: &Shared,
    sql: &str,
    sink: &mut StreamSink,
) -> DbResult<()> {
    let verbs = statement_verbs(sql);
    let mut result = conn.query_iter(sql).await.map_err(|e| to_error(&e))?;
    let mut index = 0;
    while result.columns().is_some() {
        let verb = verbs
            .get(index)
            .or(verbs.last())
            .cloned()
            .unwrap_or_default();
        match result.columns().filter(|c| !c.is_empty()) {
            Some(columns) => {
                let binary: Vec<bool> = columns.iter().map(is_binary).collect();
                let names: Vec<Column> = columns
                    .iter()
                    .map(|c| Column::new(c.name_str().into_owned(), type_name(c)))
                    .collect();
                sink.begin(&names);
                while let Some(row) = result.next().await.map_err(|e| to_error(&e))? {
                    if sink.keeping() {
                        for i in 0..row.len() {
                            let text = cell(
                                row.as_ref(i).unwrap_or(&Value::NULL),
                                binary.get(i).copied().unwrap_or(false),
                            );
                            sink.cell(text.as_deref());
                        }
                    }
                    sink.end_row();
                    if !sink.is_open() {
                        kill_query(state, None).await;
                        // Ends with the KILL's error (or the last rows); either way nothing is left.
                        let _ = result.drop_result().await;
                        return Err(DbError::Cancelled);
                    }
                }
                let seen = sink.seen();
                sink.end(&format!("SELECT {seen}"));
                if verb != "CALL" {
                    index += 1;
                }
            }
            None => {
                let affected = result.affected_rows();
                let done = status(&verb, affected);
                sink.command(&done.command, done.total_rows);
                index += 1;
                result
                    .reduce((), |_, _: Row| ())
                    .await
                    .map_err(|e| to_error(&e))?;
            }
        }
        if result.columns().is_none() {
            break;
        }
    }
    result.drop_result().await.map_err(|e| to_error(&e))?;
    Ok(())
}

/// Reads every result set of `sql`.
async fn read_all(conn: &mut Conn, sql: &str) -> DbResult<Vec<QueryResult>> {
    let verbs = statement_verbs(sql);
    let mut results: Vec<QueryResult> = Vec::new();
    let mut result = conn.query_iter(sql).await.map_err(|e| to_error(&e))?;
    let mut index = 0;
    while result.columns().is_some() {
        let verb = verbs
            .get(index)
            .or(verbs.last())
            .cloned()
            .unwrap_or_default();
        let columns = result.columns();
        match columns.filter(|c| !c.is_empty()) {
            Some(columns) => {
                let binary: Vec<bool> = columns.iter().map(is_binary).collect();
                let names: Vec<Column> = columns
                    .iter()
                    .map(|c| Column::new(c.name_str().into_owned(), type_name(c)))
                    .collect();
                let (rows, total) = result
                    .reduce(
                        (Vec::<Vec<Option<String>>>::new(), 0usize),
                        |(mut rows, total), row: Row| {
                            if rows.len() < ROW_LIMIT {
                                rows.push(
                                    (0..row.len())
                                        .map(|i| {
                                            cell(
                                                row.as_ref(i).unwrap_or(&Value::NULL),
                                                binary.get(i).copied().unwrap_or(false),
                                            )
                                        })
                                        .collect(),
                                );
                            }
                            (rows, total + 1)
                        },
                    )
                    .await
                    .map_err(|e| to_error(&e))?;
                let mut set = QueryResult::rows(names, rows, format!("SELECT {total}"));
                set.truncated = total > ROW_LIMIT;
                set.total_rows = total as u64;
                results.push(set);
                // A procedure's result sets come before its own status.
                if verb != "CALL" {
                    index += 1;
                }
            }
            None => {
                let affected = result.affected_rows();
                results.push(status(&verb, affected));
                index += 1;
                result
                    .reduce((), |_, _: Row| ())
                    .await
                    .map_err(|e| to_error(&e))?;
            }
        }
        // After a set is read the next one is already pending, even when it is
        // only a status (`is_empty` would say no: it ignores such a set).
        if result.columns().is_none() {
            break;
        }
    }
    // An error in a later statement shows up only when the rest is drained.
    result.drop_result().await.map_err(|e| to_error(&e))?;
    Ok(results)
}

#[async_trait]
impl DatabaseSession for MySqlSession {
    fn engine(&self) -> Engine {
        Engine::Mysql
    }

    fn server_version(&self) -> String {
        self.server_version.clone()
    }

    /// Stops the running query: a second, short connection sends KILL QUERY.
    /// Never waits for the connection's lock.
    /// Only the statement running when it is called is killed: the check runs
    /// again right before KILL QUERY goes out, after the second connection is up.
    fn cancel(&self) {
        let Some(op) = self.state.activity.request_cancel() else {
            return;
        };
        let state = self.state.clone();
        let kill = async move { kill_query(&state, Some(op)).await };
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(kill);
            }
            Err(_) => {
                std::thread::spawn(move || {
                    if let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                    {
                        runtime.block_on(kill);
                    }
                });
            }
        }
    }

    fn quote(&self, identifier: &str) -> String {
        quote_name(identifier)
    }

    async fn execute(&self, sql: &str) -> DbResult<Execution> {
        let start = Instant::now();
        let mut conn = self.guard().await?;
        let results = self.run_all(&mut conn, sql).await;
        self.note_transaction(&mut conn, sql).await;
        self.note_scope(&mut conn, sql).await;
        Ok(Execution::new(results?, start.elapsed()))
    }

    /// One statement in a read-only session and transaction, rolled back. DDL
    /// is refused too: it needs a read-write session.
    async fn read_only(&self, text: &str) -> DbResult<Execution> {
        let statements = sql_script::statements(text, Dialect::Mysql);
        if statements.len() > 1 {
            return Err(DbError::msg("Run one statement at a time."));
        }
        if statements.is_empty() {
            return Err(DbError::msg("There is no statement to run."));
        }
        let start = Instant::now();
        let mut conn = self.guard().await?;
        self.require_no_transaction(&mut conn).await?;
        self.silent(&mut conn, "set session transaction read only")
            .await?;
        let outcome = async {
            self.silent(&mut conn, "start transaction read only")
                .await?;
            self.run_all(&mut conn, &statements[0]).await
        }
        .await;
        let _ = self.silent(&mut conn, "rollback").await;
        let _ = self
            .silent(&mut conn, "set session transaction read write")
            .await;
        Ok(Execution::new(outcome?, start.elapsed()))
    }

    async fn rehearse(&self, text: &str) -> DbResult<Execution> {
        self.in_transaction(text, false).await
    }

    async fn execute_atomically(&self, text: &str) -> DbResult<Execution> {
        self.in_transaction(text, true).await
    }

    /// Tables and views in every database the user can see, the URL's database first.
    async fn relations(&self) -> DbResult<Vec<Relation>> {
        let sql = format!(
            "select TABLE_SCHEMA, TABLE_NAME, TABLE_TYPE, TABLE_ROWS from information_schema.TABLES where TABLE_SCHEMA not in {SYSTEM_SCHEMAS}"
        );
        let mut conn = self.guard().await?;
        let rows = self.single(&mut conn, &sql).await?.rows;
        let text =
            |r: &Vec<Option<String>>, i: usize| r.get(i).cloned().flatten().unwrap_or_default();
        let mut relations: Vec<Relation> = rows
            .iter()
            .map(|r| {
                let kind = if text(r, 2).ends_with("VIEW") {
                    RelationKind::View
                } else {
                    RelationKind::Table
                };
                Relation::new(
                    text(r, 0),
                    text(r, 1),
                    kind,
                    text(r, 3).parse().unwrap_or(-1.0),
                )
            })
            .collect();
        let first = self.database.clone().unwrap_or_default();
        relations.sort_by(|a, b| {
            ((a.schema != first), &a.schema, &a.name).cmp(&(
                (b.schema != first),
                &b.schema,
                &b.name,
            ))
        });
        Ok(relations)
    }

    async fn page(
        &self,
        relation: &Relation,
        limit: u32,
        offset: u64,
        search: &str,
        sort: Option<&ColumnSort>,
    ) -> DbResult<QueryResult> {
        let mut conn = self.guard().await?;
        self.page_in(&mut conn, relation, limit, offset, search, sort, None)
            .await
    }

    async fn page_block(
        &self,
        relation: &Relation,
        offset: u64,
        limit: u32,
        search: &str,
        sort: Option<&ColumnSort>,
        hint: Option<&KeyHint>,
    ) -> DbResult<QueryResult> {
        let mut conn = self.guard().await?;
        self.page_in(&mut conn, relation, limit, offset, search, sort, hint)
            .await
    }

    fn transaction_open(&self) -> bool {
        self.state.in_transaction.load(Ordering::SeqCst)
    }

    fn scope_changed(&self) -> bool {
        self.state.scope_changed.load(Ordering::SeqCst)
    }

    async fn execute_stream(&self, sql: &str, sink: &mut StreamSink) -> DbResult<()> {
        let mut conn = self.guard().await?;
        let id = self.state.activity.start();
        let outcome = stream_all(&mut conn, &self.state, sql, sink).await;
        let cancelled = self.state.activity.finish(id);
        self.note_transaction(&mut conn, sql).await;
        self.note_scope(&mut conn, sql).await;
        if cancelled {
            return Err(DbError::Cancelled);
        }
        outcome
    }

    /// Exact row count, or None when the table is big enough that counting is slow.
    async fn count(&self, relation: &Relation, search: &str) -> DbResult<Option<u64>> {
        if relation.estimate > COUNT_LIMIT {
            return Ok(None);
        }
        let mut conn = self.guard().await?;
        let where_ = if search.is_empty() {
            String::new()
        } else {
            let names: Vec<String> = self
                .column_types(&mut conn, relation)
                .await?
                .into_iter()
                .map(|t| t.0)
                .collect();
            let off = no_backslash_escapes(&mut conn).await?;
            Self::filter(search, &names, off)
        };
        let result = self
            .single(
                &mut conn,
                &format!("select count(*) from {}{where_}", self.qualified(relation)),
            )
            .await?;
        Ok(result
            .rows
            .first()
            .and_then(|r| r.first().cloned().flatten())
            .and_then(|v| v.parse().ok()))
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
        let mut conn = self.guard().await?;
        let off = no_backslash_escapes(&mut conn).await?;
        let target = self.qualified(relation);
        let set = changes
            .iter()
            .map(|c| {
                format!(
                    "{} = {}",
                    quote_name(&c.column),
                    literal(c.value.as_deref(), off)
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        let set_logged = changes
            .iter()
            .map(|c| format!("{} = ?", quote_name(&c.column)))
            .collect::<Vec<_>>()
            .join(", ");
        let forms = self.key_forms(&mut conn, relation, &key.columns).await?;
        let (w_sent, w_logged) = Self::where_clause(key, &forms, off)?;
        self.edit(
            &mut conn,
            &format!("update {target} set {set} where {w_sent}"),
            format!("update {target} set {set_logged} where {w_logged}"),
            true,
        )
        .await
    }

    async fn insert(&self, relation: &Relation, values: &[CellValue]) -> DbResult<EditOutcome> {
        let mut conn = self.guard().await?;
        let off = no_backslash_escapes(&mut conn).await?;
        let head = format!("insert into {}", self.qualified(relation));
        if values.is_empty() {
            let sql = format!("{head} () values ()");
            return self.edit(&mut conn, &sql, sql.clone(), false).await;
        }
        let names = values
            .iter()
            .map(|c| quote_name(&c.column))
            .collect::<Vec<_>>()
            .join(", ");
        let sent = values
            .iter()
            .map(|c| literal(c.value.as_deref(), off))
            .collect::<Vec<_>>()
            .join(", ");
        let logged = vec!["?"; values.len()].join(", ");
        self.edit(
            &mut conn,
            &format!("{head} ({names}) values ({sent})"),
            format!("{head} ({names}) values ({logged})"),
            false,
        )
        .await
    }

    async fn delete(&self, relation: &Relation, key: &RowKey) -> DbResult<EditOutcome> {
        let mut conn = self.guard().await?;
        let off = no_backslash_escapes(&mut conn).await?;
        let head = format!("delete from {} where ", self.qualified(relation));
        let forms = self.key_forms(&mut conn, relation, &key.columns).await?;
        let (sent, logged) = Self::where_clause(key, &forms, off)?;
        self.edit(
            &mut conn,
            &format!("{head}{sent}"),
            format!("{head}{logged}"),
            true,
        )
        .await
    }

    async fn schema_graph(&self) -> DbResult<SchemaGraph> {
        let columns = format!(
            "select c.table_schema, c.table_name, t.table_type = 'VIEW', c.column_name, c.column_type, \
                    c.is_nullable = 'YES', c.column_key = 'PRI' \
             from information_schema.columns c \
             join information_schema.tables t on t.table_schema = c.table_schema and t.table_name = c.table_name \
             where c.table_schema not in {SYSTEM_SCHEMAS} \
             order by c.table_schema = database() desc, c.table_schema, c.table_name, c.ordinal_position"
        );
        // 0x1F is a binary string: convert the joined names back to text.
        let links = format!(
            "select constraint_name, concat(table_schema, '.', table_name), \
                    concat(referenced_table_schema, '.', referenced_table_name), \
                    convert(group_concat(column_name order by ordinal_position separator 0x1F) using utf8mb4), \
                    convert(group_concat(referenced_column_name order by ordinal_position separator 0x1F) using utf8mb4) \
             from information_schema.key_column_usage \
             where referenced_table_name is not null and table_schema not in {SYSTEM_SCHEMAS} \
             group by table_schema, table_name, constraint_name, referenced_table_schema, referenced_table_name \
             order by 1"
        );
        let mut conn = self.guard().await?;
        let columns = self.single(&mut conn, &columns).await?.rows;
        let links = self.single(&mut conn, &links).await?.rows;
        Ok(SchemaGraph::build(&columns, &links))
    }

    async fn close(&self) {
        if let Some(conn) = self.conn.lock().await.take() {
            let _ = conn.disconnect().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn query(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn reads_ssl_modes() {
        assert_eq!(
            ssl_mode(&query(&[("ssl-mode", "required")])),
            Some("REQUIRED")
        );
        assert_eq!(
            ssl_mode(&query(&[("sslMode", "verify-full")])),
            Some("VERIFY_IDENTITY")
        );
        assert_eq!(
            ssl_mode(&query(&[("sslmode", "verify_ca")])),
            Some("VERIFY_CA")
        );
        assert_eq!(ssl_mode(&query(&[("ssl", "false")])), Some("DISABLED"));
        assert_eq!(ssl_mode(&query(&[("useSSL", "true")])), Some("REQUIRED"));
        assert_eq!(
            ssl_mode(&query(&[
                ("useSSL", "true"),
                ("verifyServerCertificate", "true")
            ])),
            Some("VERIFY_CA")
        );
        assert_eq!(ssl_mode(&query(&[("ssl-mode", "nonsense")])), None);
        assert_eq!(ssl_mode(&query(&[])), None);
    }

    #[test]
    fn builds_targets() {
        let t = Target::new(
            "mysql://ana:p%40ss@db.example.com:3307/shop?ssl-mode=REQUIRED",
            None,
        )
        .unwrap();
        assert_eq!((t.host.as_str(), t.port), ("db.example.com", 3307));
        assert_eq!(t.user.as_deref(), Some("ana"));
        assert_eq!(t.password.as_deref(), Some("p@ss"));
        assert_eq!(t.database.as_deref(), Some("shop"));
        assert_eq!(t.ssl_mode, Some("REQUIRED"));
        let t = Target::new("mariadb://root@/", None).unwrap();
        assert_eq!((t.host.as_str(), t.port), ("localhost", 3306));
        assert_eq!(t.database, None);
        let t = Target::new("mysql://ana@db.example.com/shop", Some(40000)).unwrap();
        assert_eq!((t.host.as_str(), t.port), ("127.0.0.1", 40000));
        assert_eq!(t.tls_name.as_deref(), Some("db.example.com"));
        assert!(Target::new("postgres://x@y/z", None).is_err());
    }

    #[test]
    fn trims_versions() {
        assert_eq!(version("8.4.3"), "8.4.3");
        assert_eq!(version("11.4.2-MariaDB-ubu2404"), "11.4.2-MariaDB");
    }

    #[test]
    fn finds_statement_verbs() {
        assert_eq!(
            statement_verbs("insert into t values (';'); -- x\n select 1; /* c */ call p()"),
            vec!["INSERT", "SELECT", "CALL"]
        );
        assert_eq!(
            statement_verbs("  # note\n update t set a = 'it\\'s; x'"),
            vec!["UPDATE"]
        );
        assert_eq!(statement_verbs("select 1;"), vec!["SELECT"]);
        assert!(statement_verbs("-- nothing").is_empty());
    }

    #[test]
    fn knows_implicit_commits() {
        assert!(commits_by_itself("alter table t add c int"));
        assert!(commits_by_itself("/* x */ DROP TABLE t"));
        assert!(!commits_by_itself("create temporary table t (a int)"));
        assert!(!commits_by_itself("update t set a = 1"));
    }

    #[test]
    fn writes_hex() {
        assert_eq!(hex(&[0xDE, 0xAD, 0x01]), "0xDEAD01");
        assert_eq!(hex(&[0xAB; 65]), format!("0x{}…", "AB".repeat(64)));
    }

    #[test]
    fn escapes_literals() {
        assert_eq!(literal(None, false), "NULL");
        assert_eq!(literal(Some("it's"), false), "'it''s'");
        assert_eq!(literal(Some("a\\b\n\"\0"), false), "'a\\\\b\\n\\\"\\0'");
        assert_eq!(literal(Some("a\\b'"), true), "'a\\b'''");
    }

    #[test]
    fn labels_statuses() {
        assert_eq!(status("UPDATE", 2).command, "UPDATE 2");
        assert_eq!(status("CREATE", 0).command, "CREATE");
        assert_eq!(status("", 0).command, "OK");
    }

    #[test]
    fn builds_where_clauses() {
        let key = RowKey {
            columns: vec!["a".into(), "b".into(), "c".into()],
            kind: RowKeyKind::PrimaryKey,
            values: vec![Some("1".into()), None, Some("x'y".into())],
        };
        let (sent, logged) = MySqlSession::where_clause(&key, &[], false).unwrap();
        assert_eq!(sent, "`a` = '1' and `c` = 'x''y' and `b` is null");
        assert_eq!(logged, "`a` = ? and `c` = ? and `b` is null");
        let exact = RowKey {
            columns: vec!["u".into(), "f".into(), "b".into()],
            kind: RowKeyKind::PrimaryKey,
            values: vec![
                Some("0001ABff".into()),
                Some("0.1".into()),
                Some("5".into()),
            ],
        };
        let forms = [KeyForm::Hex, KeyForm::FloatText, KeyForm::Number];
        let (sent, _) = MySqlSession::where_clause(&exact, &forms, false).unwrap();
        assert_eq!(
            sent,
            "`u` = x'0001ABff' and cast(`f` as char) = '0.1' and `b` = 5"
        );
        let bad = RowKey {
            values: vec![Some("0x01…".into()), Some("1".into()), Some("1".into())],
            ..exact.clone()
        };
        assert!(MySqlSession::where_clause(&bad, &forms, false).is_err());
        let injected = RowKey {
            values: vec![Some("00".into()), Some("1".into()), Some("1 or 1=1".into())],
            ..exact
        };
        assert!(MySqlSession::where_clause(&injected, &forms, false).is_err());
    }

    #[test]
    fn picks_key_forms_from_column_types() {
        assert_eq!(key_form("binary(16)"), KeyForm::Hex);
        assert_eq!(key_form("varbinary(255)"), KeyForm::Hex);
        assert_eq!(key_form("bit(5)"), KeyForm::Number);
        assert_eq!(key_form("float unsigned"), KeyForm::FloatText);
        assert_eq!(key_form("double(10,2)"), KeyForm::FloatText);
        assert_eq!(key_form("int(11)"), KeyForm::Text);
        assert_eq!(key_form("varchar(40)"), KeyForm::Text);
    }

    #[test]
    fn quotes_names() {
        assert_eq!(quote_name("we`ird"), "`we``ird`");
    }
}
