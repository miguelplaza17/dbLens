//! Postgres driver on tokio-postgres + rustls (ring). Port of `Postgres.swift`.
//!
//! Reads go through the simple query protocol, so every value arrives as the
//! text the server prints (what libpq gives the Swift app) and NULL is `None`.
//! Edits send their values as text parameters and the server casts each one to
//! the column's type. One operation runs at a time (`gate`), because a
//! transaction spans several calls; `cancel` never waits for it.

use std::error::Error as StdError;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bytes::{BufMut, BytesMut};
use futures_util::TryStreamExt;
use tokio::sync::{Mutex, MutexGuard};
use tokio::task::JoinHandle;
use tokio_postgres::config::{ChannelBinding, SslMode as PgSslMode, TargetSessionAttrs};
use tokio_postgres::error::SqlState;
use tokio_postgres::types::{to_sql_checked, Format, IsNull, ToSql, Type};
use tokio_postgres::{CancelToken, Client, Config, SimpleQueryMessage};
use tokio_postgres_rustls::MakeRustlsConnect;

use crate::connector::Summary;
use crate::engine::Engine;
use crate::session::{
    keyset_direction, like_pattern, Activity, CellValue, Column, ColumnSort, DatabaseSession,
    DbError, DbResult, EditOutcome, Execution, KeyHint, QueryResult, Relation, RelationKind,
    RowKey, RowKeyKind, RowKeys, SchemaGraph, DEEP_OFFSET, ROW_LIMIT,
};
use crate::sql_script::{self, Dialect};
use crate::url::{quote_ident, ConnectionUrl};
use crate::wire::StreamSink;

/// Host, database, user and TLS mode for the connection sheet.
pub fn summary(text: &str) -> Option<Summary> {
    let url = ConnectionUrl::new(text).ok()?;
    Some(Summary {
        engine: Engine::Postgres,
        database: url.database(),
        host: Some(url.host()),
        port: url.port(),
        user: url.user().map(String::from),
        tls: url.sslmode().map(String::from),
        suggested_name: url.suggested_name(),
    })
}

// MARK: - Connecting

/// libpq's `sslmode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ssl {
    Disable,
    /// Plain first in libpq; here the same as `Prefer` (TLS when the server offers it).
    Allow,
    Prefer,
    /// Encrypted, certificate not checked (as in libpq).
    Require,
    /// Certificate must chain to a trusted root; the host name is not checked.
    VerifyCa,
    VerifyFull,
}

impl Ssl {
    fn parse(text: &str) -> DbResult<Ssl> {
        Ok(match text.trim().to_lowercase().as_str() {
            "disable" => Ssl::Disable,
            "allow" => Ssl::Allow,
            "prefer" | "" => Ssl::Prefer,
            "require" => Ssl::Require,
            "verify-ca" => Ssl::VerifyCa,
            "verify-full" => Ssl::VerifyFull,
            other => return Err(DbError::msg(format!("invalid sslmode value: \"{other}\""))),
        })
    }

    fn pg(self) -> PgSslMode {
        match self {
            Ssl::Disable => PgSslMode::Disable,
            Ssl::Allow | Ssl::Prefer => PgSslMode::Prefer,
            Ssl::Require | Ssl::VerifyCa | Ssl::VerifyFull => PgSslMode::Require,
        }
    }
}

/// What the TLS side of a connection needs from the URL.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TlsSpec {
    mode: Ssl,
    root_cert: Option<String>,
}

/// The driver's settings from the URL, and the TLS choice. `local_port` aims the
/// connection at an SSH tunnel while the URL's host stays the TLS server name.
fn build_config(url: &ConnectionUrl, local_port: Option<u16>) -> DbResult<(Config, TlsSpec)> {
    let setting = |key: &str| {
        url.settings
            .get(key)
            .map(String::as_str)
            .filter(|v| !v.is_empty())
    };
    let mut config = Config::new();

    let user = setting("user")
        .map(String::from)
        .or_else(|| std::env::var("PGUSER").ok())
        .or_else(|| std::env::var("USER").ok())
        .or_else(|| std::env::var("USERNAME").ok())
        .unwrap_or_else(|| "postgres".into());
    config.user(&user);
    if let Some(password) = setting("password")
        .map(String::from)
        .or_else(|| std::env::var("PGPASSWORD").ok())
    {
        config.password(password);
    }
    if let Some(dbname) = setting("dbname") {
        config.dbname(dbname);
    }
    config.application_name(setting("application_name").unwrap_or("dbLens"));
    if let Some(options) = setting("options") {
        config.options(options);
    }

    match local_port {
        Some(port) => {
            // The tunnel's end is on this computer; the name stays the URL's for TLS.
            let host = url.host();
            let host = if host.starts_with('/') {
                "localhost".to_string()
            } else {
                host
            };
            config
                .host(&host)
                .hostaddr("127.0.0.1".parse().expect("loopback"))
                .port(port);
        }
        None => {
            let hosts: Vec<&str> = url
                .hosts()
                .split(',')
                .map(str::trim)
                .filter(|h| !h.is_empty())
                .collect();
            if hosts.is_empty() {
                if cfg!(unix) {
                    config.host("/var/run/postgresql").host("/tmp");
                } else {
                    config.host("localhost");
                }
            }
            for host in hosts {
                config.host(host);
            }
            if let Some(hostaddr) =
                setting("hostaddr").and_then(|a| a.split(',').next()?.trim().parse().ok())
            {
                config.hostaddr(hostaddr);
            }
            let ports: Vec<u16> = setting("port")
                .map(|p| p.split(',').filter_map(|p| p.trim().parse().ok()).collect())
                .unwrap_or_default();
            for port in ports {
                config.port(port);
            }
        }
    }

    let timeout = setting("connect_timeout")
        .and_then(|t| t.parse::<u64>().ok())
        .unwrap_or(10);
    if timeout > 0 {
        config.connect_timeout(Duration::from_secs(timeout.max(2)));
    }
    config.keepalives(true);
    match setting("target_session_attrs") {
        Some("read-write") => {
            config.target_session_attrs(TargetSessionAttrs::ReadWrite);
        }
        Some("read-only") => {
            config.target_session_attrs(TargetSessionAttrs::ReadOnly);
        }
        _ => {}
    }
    match setting("channel_binding") {
        Some("disable") => {
            config.channel_binding(ChannelBinding::Disable);
        }
        Some("require") => {
            config.channel_binding(ChannelBinding::Require);
        }
        _ => {}
    }

    let mode = match (setting("sslmode"), setting("requiressl")) {
        (Some(mode), _) => Ssl::parse(mode)?,
        (None, Some("1")) => Ssl::Require,
        _ => Ssl::Prefer,
    };
    config.ssl_mode(mode.pg());
    Ok((
        config,
        TlsSpec {
            mode,
            root_cert: setting("sslrootcert").map(String::from),
        },
    ))
}

/// Accepts any certificate (`require`) or any that chains to a trusted root
/// regardless of the host name (`verify-ca`), the way libpq does.
#[derive(Debug)]
struct LenientVerifier {
    chain: Option<Arc<rustls::client::WebPkiServerVerifier>>,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl rustls::client::danger::ServerCertVerifier for LenientVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        intermediates: &[rustls::pki_types::CertificateDer<'_>],
        server_name: &rustls::pki_types::ServerName<'_>,
        ocsp_response: &[u8],
        now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        let Some(chain) = &self.chain else {
            return Ok(rustls::client::danger::ServerCertVerified::assertion());
        };
        match chain.verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now) {
            Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::NotValidForName
                | rustls::CertificateError::NotValidForNameContext { .. },
            )) => Ok(rustls::client::danger::ServerCertVerified::assertion()),
            other => other,
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// The trusted roots: the file named by `sslrootcert` (PEM), or Mozilla's list.
fn root_store(spec: &TlsSpec) -> DbResult<rustls::RootCertStore> {
    use rustls::pki_types::{pem::PemObject, CertificateDer};
    let mut roots = rustls::RootCertStore::empty();
    match spec.root_cert.as_deref() {
        Some(path) if path != "system" => {
            let bytes = std::fs::read(path).map_err(|e| {
                DbError::msg(format!(
                    "could not read root certificate file \"{path}\": {e}"
                ))
            })?;
            let certs = CertificateDer::pem_slice_iter(&bytes)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| {
                    DbError::msg(format!(
                        "could not read root certificate file \"{path}\": {e}"
                    ))
                })?;
            let (added, _) = roots.add_parsable_certificates(certs);
            if added == 0 {
                return Err(DbError::msg(format!(
                    "no certificate found in root certificate file \"{path}\""
                )));
            }
        }
        _ => roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned()),
    }
    Ok(roots)
}

fn make_tls(spec: &TlsSpec) -> DbResult<MakeRustlsConnect> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let tls_error = |e: &dyn std::fmt::Display| DbError::msg(format!("could not set up TLS: {e}"));
    let builder = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| tls_error(&e))?;
    let config = match spec.mode {
        Ssl::VerifyFull => builder
            .with_root_certificates(root_store(spec)?)
            .with_no_client_auth(),
        Ssl::VerifyCa => {
            let chain = rustls::client::WebPkiServerVerifier::builder_with_provider(
                Arc::new(root_store(spec)?),
                provider.clone(),
            )
            .build()
            .map_err(|e| tls_error(&e))?;
            builder
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(LenientVerifier {
                    chain: Some(chain),
                    provider,
                }))
                .with_no_client_auth()
        }
        // Disable never reaches the handshake; the rest take what the server shows.
        _ => builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(LenientVerifier {
                chain: None,
                provider,
            }))
            .with_no_client_auth(),
    };
    Ok(MakeRustlsConnect::new(config))
}

/// Opens a session. Through an SSH tunnel, `local_port` replaces the URL's host and port.
pub async fn connect(url: &str, local_port: Option<u16>) -> DbResult<Box<dyn DatabaseSession>> {
    let url = ConnectionUrl::new(url)?;
    let (config, spec) = build_config(&url, local_port)?;
    let tls = make_tls(&spec)?;
    let (client, connection) = config
        .connect(tls.clone())
        .await
        .map_err(|e| DbError::Message(describe(&e)))?;
    let task = tokio::spawn(async move {
        let _ = connection.await;
    });

    let (version, scope) = match client
        .simple_query(&format!(
            "select current_setting('server_version_num'), {SCOPE}"
        ))
        .await
    {
        Ok(messages) => {
            let row = messages.iter().find_map(|m| match m {
                SimpleQueryMessage::Row(row) => Some(row),
                _ => None,
            });
            let version = row
                .and_then(|r| r.get(0))
                .and_then(|v| v.parse::<u64>().ok())
                .map(format_version)
                .unwrap_or_default();
            let scope = row.and_then(|r| r.get(1)).unwrap_or_default().to_string();
            (version, scope)
        }
        Err(error) => {
            task.abort();
            return Err(DbError::Message(describe(&error)));
        }
    };

    Ok(Box::new(PostgresSession {
        cancel_token: client.cancel_token(),
        client,
        gate: Mutex::new(()),
        tls,
        runtime: tokio::runtime::Handle::current(),
        activity: Arc::new(Activity::default()),
        connection: StdMutex::new(Some(task)),
        server_version: version,
        in_transaction: AtomicBool::new(false),
        scope,
        scope_changed: AtomicBool::new(false),
    }))
}

/// 160004 is "16.4"; 90624 (9.6.24) is "9.624" like the Swift app prints it.
fn format_version(number: u64) -> String {
    format!("{}.{}", number / 10_000, number % 10_000)
}

// MARK: - Errors

/// The server's own text the way libpq prints it ("ERROR:  …", then DETAIL and
/// HINT), or the driver's message with its causes.
fn describe(error: &tokio_postgres::Error) -> String {
    if let Some(db) = error.as_db_error() {
        let mut text = format!("{}:  {}", db.severity(), db.message());
        if let Some(detail) = db.detail() {
            text.push_str(&format!("\nDETAIL:  {detail}"));
        }
        if let Some(hint) = db.hint() {
            text.push_str(&format!("\nHINT:  {hint}"));
        }
        return text;
    }
    let mut text = error.to_string();
    let mut cause = error.source();
    while let Some(next) = cause {
        text.push_str(": ");
        text.push_str(&next.to_string());
        cause = next.source();
    }
    text
}

/// A value sent as text; the server reads it as the parameter's type. None is NULL.
#[derive(Debug)]
struct TextParam(Option<String>);

impl ToSql for TextParam {
    fn to_sql(
        &self,
        _ty: &Type,
        out: &mut BytesMut,
    ) -> Result<IsNull, Box<dyn StdError + Sync + Send>> {
        match &self.0 {
            Some(text) => {
                out.put_slice(text.as_bytes());
                Ok(IsNull::No)
            }
            None => Ok(IsNull::Yes),
        }
    }

    fn accepts(_ty: &Type) -> bool {
        true
    }

    fn encode_format(&self, _ty: &Type) -> Format {
        Format::Text
    }

    to_sql_checked!();
}

// MARK: - SQL text helpers

/// A string literal that is safe whatever `standard_conforming_strings` says.
fn literal(text: &str) -> String {
    if text.contains('\\') {
        format!("E'{}'", text.replace('\\', "\\\\").replace('\'', "''"))
    } else {
        format!("'{}'", text.replace('\'', "''"))
    }
}

/// The command tag the server would have sent ("UPDATE 2", "CREATE TABLE"); the
/// simple query protocol only reports the row count.
fn command_tag(statement: &str, rows: u64, has_columns: bool) -> String {
    let first = sql_script::verb(statement, 1);
    match first.as_str() {
        "" => String::new(),
        "SELECT" | "VALUES" | "TABLE" => format!("SELECT {rows}"),
        "INSERT" => format!("INSERT 0 {rows}"),
        "UPDATE" | "DELETE" | "MERGE" | "MOVE" | "FETCH" | "COPY" => format!("{first} {rows}"),
        "WITH" => {
            let lower = statement.to_lowercase();
            let write = ["insert", "update", "delete", "merge"].iter().find(|verb| {
                lower
                    .split(|c: char| !c.is_alphanumeric() && c != '_')
                    .any(|word| word == **verb)
            });
            match write {
                Some(verb) if !has_columns || lower.contains("returning") => {
                    let upper = verb.to_uppercase();
                    if upper == "INSERT" {
                        format!("INSERT 0 {rows}")
                    } else {
                        format!("{upper} {rows}")
                    }
                }
                _ => format!("SELECT {rows}"),
            }
        }
        "CREATE" | "ALTER" | "DROP" => {
            let words = sql_script::verb(statement, 8);
            let noise = [
                "OR",
                "REPLACE",
                "TEMP",
                "TEMPORARY",
                "UNLOGGED",
                "UNIQUE",
                "GLOBAL",
                "LOCAL",
                "RECURSIVE",
                "CONCURRENTLY",
            ];
            let rest: Vec<&str> = words
                .split(' ')
                .skip(1)
                .filter(|w| !noise.contains(w))
                .collect();
            match rest.as_slice() {
                ["MATERIALIZED", "VIEW", ..] => format!("{first} MATERIALIZED VIEW"),
                ["FOREIGN", "TABLE", ..] => format!("{first} FOREIGN TABLE"),
                ["FOREIGN", "DATA", ..] => format!("{first} FOREIGN DATA WRAPPER"),
                ["EVENT", "TRIGGER", ..] => format!("{first} EVENT TRIGGER"),
                [object, ..] => format!("{first} {object}"),
                [] => first,
            }
        }
        "TRUNCATE" => "TRUNCATE TABLE".into(),
        "LOCK" => "LOCK TABLE".into(),
        "START" => "START TRANSACTION".into(),
        _ => first,
    }
}

/// `where` text for a row key, numbering parameters after `after` others.
fn key_filter(key: &RowKey, after: usize) -> DbResult<(String, Vec<Option<String>>)> {
    if key.columns.is_empty() || key.columns.len() != key.values.len() {
        return Err(DbError::msg(
            "The row has no key to find it again. Reload the table.",
        ));
    }
    if key.kind == RowKeyKind::Rowid {
        return Err(DbError::msg(
            "A rowid does not identify rows in PostgreSQL.",
        ));
    }
    let parts: Vec<String> = key
        .columns
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let n = after + index + 1;
            if key.kind != RowKeyKind::Ctid {
                format!("{} = ${n}", quote_ident(name))
            } else if name == "ctid" {
                format!("ctid = ${n}::tid")
            } else {
                format!("xmin::text = ${n}")
            }
        })
        .collect();
    Ok((parts.join(" and "), key.values.clone()))
}

/// "only " for a plain table, so an edit never reaches a table that inherits from it.
fn only(relation: &Relation) -> &'static str {
    if relation.kind == RelationKind::Table {
        "only "
    } else {
        ""
    }
}

fn relation_kind(code: &str) -> RelationKind {
    match code {
        "v" => RelationKind::View,
        "m" => RelationKind::Materialized,
        "f" => RelationKind::Foreign,
        "p" => RelationKind::Partitioned,
        _ => RelationKind::Table,
    }
}

// MARK: - Session

pub struct PostgresSession {
    client: Client,
    /// One operation at a time: a transaction spans several calls.
    gate: Mutex<()>,
    cancel_token: CancelToken,
    tls: MakeRustlsConnect,
    runtime: tokio::runtime::Handle,
    /// The running cancellable operation. `cancel` only reaches that one, and a
    /// 57014 reads as cancelled only when it was asked for (one from a
    /// statement_timeout stays the server's message).
    activity: Arc<Activity>,
    connection: StdMutex<Option<JoinHandle<()>>>,
    server_version: String,
    /// The user's transaction is open, as of the last SQL run.
    in_transaction: AtomicBool,
    /// `SCOPE` right after connecting.
    scope: String,
    /// `SCOPE` differs from `scope`, as of the last SQL run.
    scope_changed: AtomicBool,
}

/// What other connections to the same URL would not share: the role, the
/// search path and whether this backend has temp tables.
const SCOPE: &str = "current_user || '|' || current_setting('search_path') || '|' || \
     exists(select 1 from pg_class where relnamespace = pg_my_temp_schema())";

/// Whether a run may have changed the session's role, search path or temp tables.
fn may_change_scope(statements: &[String]) -> bool {
    statements.iter().any(|s| {
        let verb = sql_script::verb(s, 1);
        let upper = s.to_uppercase();
        match verb.as_str() {
            "SET" | "RESET" => {
                sql_script::verb(s, 2) != "SET LOCAL"
                    && ["ROLE", "AUTHORIZATION", "SEARCH_PATH", "ALL"]
                        .iter()
                        .any(|w| upper.contains(w))
            }
            "DISCARD" => true,
            "CREATE" | "SELECT" => upper.contains("TEMP"),
            _ => false,
        }
    })
}

/// Whether a run may have opened a transaction block (or changed one already open).
fn may_open_transaction(statements: &[String]) -> bool {
    statements
        .iter()
        .any(|s| matches!(sql_script::verb(s, 1).as_str(), "BEGIN" | "START"))
}

/// The session's turn for one cancellable operation; it ends when dropped.
struct Turn<'a> {
    _gate: MutexGuard<'a, ()>,
    activity: &'a Activity,
    id: u64,
}

impl Drop for Turn<'_> {
    fn drop(&mut self) {
        self.activity.finish(self.id);
    }
}

/// Whether the connection is inside a transaction block (one the user opened in
/// the SQL panel).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TxState {
    Idle,
    Open,
    /// An error ended it; everything but ROLLBACK is refused.
    Failed,
}

const TRANSACTION_OPEN: &str =
    "A transaction is open in the SQL panel. Commit or roll it back first.";

impl PostgresSession {
    /// Waits for the session's turn; `cancel` can stop what runs in it.
    async fn turn(&self) -> Turn<'_> {
        let gate = self.gate.lock().await;
        let id = self.activity.start();
        Turn {
            _gate: gate,
            activity: &self.activity,
            id,
        }
    }

    /// Waits for the session's turn for an edit, which `cancel` leaves alone so it
    /// never lands between an edit's own statements.
    async fn quiet_turn(&self) -> MutexGuard<'_, ()> {
        self.gate.lock().await
    }

    /// Asks the server whether a transaction block is open: a savepoint is
    /// refused outside one (25P01), and every statement inside a failed one (25P02).
    async fn transaction_state(&self) -> DbResult<TxState> {
        match self.client.batch_execute("savepoint dblens_probe").await {
            Ok(()) => {
                let _ = self
                    .client
                    .batch_execute("release savepoint dblens_probe")
                    .await;
                Ok(TxState::Open)
            }
            Err(e) if e.code() == Some(&SqlState::NO_ACTIVE_SQL_TRANSACTION) => Ok(TxState::Idle),
            Err(e) if e.code() == Some(&SqlState::IN_FAILED_SQL_TRANSACTION) => Ok(TxState::Failed),
            Err(e) => Err(self.fail(e)),
        }
    }

    /// Refuses to start dbLens's own transaction inside the user's: its
    /// rollback would undo the user's pending work too.
    async fn require_no_transaction(&self) -> DbResult<()> {
        match self.transaction_state().await? {
            TxState::Idle => Ok(()),
            TxState::Open => Err(DbError::msg(TRANSACTION_OPEN)),
            TxState::Failed => Err(DbError::msg(
                "The open transaction failed. Run ROLLBACK in the SQL panel first.",
            )),
        }
    }

    fn fail(&self, error: tokio_postgres::Error) -> DbError {
        if error.code() == Some(&SqlState::QUERY_CANCELED) && self.activity.current_cancelled() {
            DbError::Cancelled
        } else {
            DbError::Message(describe(&error))
        }
    }

    /// Statements that return nothing (begin, commit, rollback).
    async fn command(&self, sql: &str) -> DbResult<()> {
        self.client
            .batch_execute(sql)
            .await
            .map_err(|e| self.fail(e))
    }

    /// Ends a transaction we started; a failure here changes nothing for the caller.
    async fn rollback(&self) {
        let _ = self.client.batch_execute("rollback").await;
    }

    /// Runs the text as typed (one or more statements) and reads each result.
    /// `statements` is the text split, for the command tags.
    /// The rows stream in and only the first `ROW_LIMIT` of each result are
    /// kept, so a huge SELECT never sits in memory whole.
    async fn simple(&self, sql: &str, statements: &[String]) -> DbResult<Vec<QueryResult>> {
        let stream = self
            .client
            .simple_query_raw(sql)
            .await
            .map_err(|e| self.fail(e))?;
        let mut stream = std::pin::pin!(stream);
        let mut results: Vec<QueryResult> = Vec::new();
        let mut columns: Option<Vec<Column>> = None;
        let mut rows: Vec<Vec<Option<String>>> = Vec::new();
        let mut seen: usize = 0;
        while let Some(message) = stream.try_next().await.map_err(|e| self.fail(e))? {
            match message {
                SimpleQueryMessage::RowDescription(described) => {
                    columns = Some(
                        described
                            .iter()
                            .map(|c| Column::new(c.name(), ""))
                            .collect(),
                    );
                    rows.clear();
                    seen = 0;
                }
                SimpleQueryMessage::Row(row) => {
                    seen += 1;
                    if rows.len() < ROW_LIMIT {
                        rows.push(
                            (0..row.len())
                                .map(|i| row.get(i).map(String::from))
                                .collect(),
                        );
                    }
                }
                SimpleQueryMessage::CommandComplete(count) => {
                    let statement = statements
                        .get(results.len())
                        .map(String::as_str)
                        .unwrap_or("");
                    let has_columns = columns.is_some();
                    let command = command_tag(
                        statement,
                        if has_columns { seen as u64 } else { count },
                        has_columns,
                    );
                    results.push(match columns.take() {
                        Some(columns) => QueryResult {
                            truncated: seen > rows.len(),
                            total_rows: seen as u64,
                            ..QueryResult::rows(columns, std::mem::take(&mut rows), command)
                        },
                        None => QueryResult::command(command, count),
                    });
                    seen = 0;
                }
                _ => {}
            }
        }
        Ok(results)
    }

    /// Remembers whether the user's transaction is open after a run, asking
    /// the server only when the run could have changed it.
    async fn note_transaction(&self, statements: &[String]) {
        if self.in_transaction.load(Ordering::SeqCst) || may_open_transaction(statements) {
            let open = self.transaction_state().await.map(|s| s != TxState::Idle);
            self.in_transaction
                .store(open.unwrap_or(false), Ordering::SeqCst);
        }
    }

    /// Remembers whether the panel moved the connection to another role, search
    /// path or temp tables, asking the server only when a run could have.
    async fn note_scope(&self, statements: &[String]) {
        if self.scope_changed.load(Ordering::SeqCst) || may_change_scope(statements) {
            let now = self
                .client
                .simple_query(&format!("select {SCOPE}"))
                .await
                .ok()
                .and_then(|messages| {
                    messages.iter().find_map(|m| match m {
                        SimpleQueryMessage::Row(row) => row.get(0).map(str::to_string),
                        _ => None,
                    })
                });
            if let Some(now) = now {
                self.scope_changed
                    .store(now != self.scope, Ordering::SeqCst);
            }
        }
    }

    /// `simple`, sending rows to `sink` as they arrive instead of keeping them.
    async fn simple_stream(
        &self,
        sql: &str,
        statements: &[String],
        sink: &mut StreamSink,
    ) -> DbResult<()> {
        let stream = self
            .client
            .simple_query_raw(sql)
            .await
            .map_err(|e| self.fail(e))?;
        let mut stream = std::pin::pin!(stream);
        let mut index = 0usize;
        let mut has_columns = false;
        while let Some(message) = stream.try_next().await.map_err(|e| self.fail(e))? {
            match message {
                SimpleQueryMessage::RowDescription(described) => {
                    let columns: Vec<Column> = described
                        .iter()
                        .map(|c| Column::new(c.name(), ""))
                        .collect();
                    sink.begin(&columns);
                    has_columns = true;
                }
                SimpleQueryMessage::Row(row) => {
                    if sink.keeping() {
                        for i in 0..row.len() {
                            sink.cell(row.get(i));
                        }
                    }
                    sink.end_row();
                    if !sink.is_open() {
                        // Nobody reads any more: stop the query on the server.
                        self.cancel();
                        return Err(DbError::Cancelled);
                    }
                }
                SimpleQueryMessage::CommandComplete(count) => {
                    let statement = statements.get(index).map(String::as_str).unwrap_or("");
                    if has_columns {
                        let seen = sink.seen();
                        sink.end(&command_tag(statement, seen, true));
                    } else {
                        sink.command(&command_tag(statement, count, false), count);
                    }
                    has_columns = false;
                    index += 1;
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// One statement's result.
    async fn one(&self, statement: String) -> DbResult<QueryResult> {
        let list = [statement];
        let mut results = self.simple(&list[0], &list).await?;
        Ok(results.pop().unwrap_or_else(|| QueryResult::command("", 0)))
    }

    /// The main result of a catalog query.
    async fn query(&self, sql: &str) -> DbResult<QueryResult> {
        self.one(sql.to_string()).await
    }

    /// Matches the whole row as text, so every column is searched at once.
    fn filter(search: &str) -> String {
        if search.is_empty() {
            String::new()
        } else {
            format!(" where t::text ilike {}", literal(&like_pattern(search)))
        }
    }

    async fn primary_key(&self, relation: &Relation) -> DbResult<Vec<String>> {
        if !matches!(
            relation.kind,
            RelationKind::Table | RelationKind::Partitioned
        ) {
            return Ok(vec![]);
        }
        let result = self
            .query(&format!(
                "select a.attname
                 from pg_index i
                 join lateral unnest(i.indkey) with ordinality as k(attnum, n) on true
                 join pg_attribute a on a.attrelid = i.indrelid and a.attnum = k.attnum
                 where i.indrelid = {}::regclass and i.indisprimary
                 order by k.n",
                literal(&self.qualified(relation))
            ))
            .await?;
        Ok(result
            .rows
            .into_iter()
            .filter_map(|row| row.into_iter().next().flatten())
            .collect())
    }

    /// Takes back an edit: to its savepoint inside the user's transaction, else
    /// the transaction it ran in.
    async fn undo_edit(&self, nested: bool) {
        if nested {
            let _ = self
                .client
                .batch_execute("rollback to savepoint dblens_edit; release savepoint dblens_edit")
                .await;
        } else {
            self.rollback().await;
        }
    }

    /// Runs one statement with text parameters. An edit meant for exactly one
    /// row is rolled back if it would touch more, and refused if it touches none.
    async fn edit(
        &self,
        sql: String,
        params: Vec<Option<String>>,
        expect_one: bool,
        tag: fn(u64) -> String,
    ) -> DbResult<EditOutcome> {
        // Inside the user's open transaction the edit becomes part of it, behind a
        // savepoint: a refused edit is undone alone, and the commit stays the user's.
        let nested = match self.transaction_state().await? {
            TxState::Idle => false,
            TxState::Open => true,
            TxState::Failed => {
                return Err(DbError::msg(
                    "The open transaction failed. Run ROLLBACK in the SQL panel first.",
                ))
            }
        };
        let guarded = nested || expect_one;
        if nested {
            self.command("savepoint dblens_edit").await?;
        } else if expect_one {
            self.command("begin").await?;
        }
        let params: Vec<TextParam> = params.into_iter().map(TextParam).collect();
        let refs: Vec<&(dyn ToSql + Sync)> =
            params.iter().map(|p| p as &(dyn ToSql + Sync)).collect();
        let affected = match self.client.execute(sql.as_str(), &refs).await {
            Ok(affected) => affected,
            Err(error) => {
                let error = self.fail(error);
                if guarded {
                    self.undo_edit(nested).await;
                }
                return Err(error);
            }
        };
        if expect_one {
            if affected > 1 {
                self.undo_edit(nested).await;
                return Err(DbError::msg(format!(
                    "The change would have affected {affected} rows and was rolled back."
                )));
            }
            if affected != 1 {
                self.undo_edit(nested).await;
                return Err(DbError::msg("The row changed or was deleted since it was loaded. Reload the table and try again."));
            }
        }
        if guarded {
            let end = if nested {
                "release savepoint dblens_edit"
            } else {
                "commit"
            };
            if let Err(error) = self.command(end).await {
                self.undo_edit(nested).await;
                return Err(error);
            }
        }
        Ok(EditOutcome {
            sql,
            command: tag(affected),
            affected,
        })
    }
}

impl PostgresSession {
    /// A failed run that left the transaction failed keeps it, with a hint.
    async fn after_run<T>(&self, outcome: DbResult<T>) -> DbResult<T> {
        match outcome {
            Ok(value) => Ok(value),
            Err(error) => {
                // `begin; select 1/0` leaves the transaction failed. Keep it: the
                // user may recover with `rollback to <savepoint>`, and a rollback
                // here would throw away their earlier work. Say what to run.
                if self.transaction_state().await.ok() == Some(TxState::Failed) {
                    return Err(match error {
                        DbError::Message(text) => DbError::Message(format!(
                            "{text}\n\nThe transaction is now failed. Run ROLLBACK, or ROLLBACK TO a savepoint."
                        )),
                        other => other,
                    });
                }
                Err(error)
            }
        }
    }

    /// `page`, starting from `hint` (keyset pagination) or from a key found by a
    /// key-only scan for deep offsets, when the order is the key's.
    async fn page_in(
        &self,
        relation: &Relation,
        limit: u32,
        offset: u64,
        search: &str,
        sort: Option<&ColumnSort>,
        hint: Option<&KeyHint>,
    ) -> DbResult<QueryResult> {
        let primary_key = self.primary_key(relation).await?;
        let uses_ctid = primary_key.is_empty() && relation.kind == RelationKind::Table;
        let keys: Vec<String> = if !primary_key.is_empty() {
            primary_key.iter().map(|k| quote_ident(k)).collect()
        } else if uses_ctid {
            vec!["t.ctid".into()]
        } else {
            vec![]
        };
        let mut terms: Vec<String> = sort
            .map(|s| {
                vec![format!(
                    "{} {} nulls last",
                    quote_ident(&s.column),
                    s.direction()
                )]
            })
            .unwrap_or_default();
        terms.extend(keys.iter().cloned());
        if keys.is_empty() {
            // No key and no ctid (a view, a materialized or foreign table): the
            // whole row as text breaks ties (any type has a text form), so blocks
            // fetched one by one with OFFSET neither overlap nor skip rows.
            terms.push("(t.*)::text".into());
        }
        let order = if terms.is_empty() {
            String::new()
        } else {
            format!(" order by {}", terms.join(", "))
        };
        let (where_, skip) = self
            .keyset(
                relation,
                &primary_key,
                uses_ctid,
                search,
                sort,
                &order,
                offset,
                hint,
            )
            .await?;
        // Without a primary key, a row is found by ctid, checked with xmin: if another
        // session changed or vacuumed it since, the ctid may point to another row.
        let mut result = self
            .query(&format!(
                "select {}* from {} as t{}{} limit {} offset {}",
                if uses_ctid {
                    "ctid::text, xmin::text, "
                } else {
                    ""
                },
                self.qualified(relation),
                where_,
                order,
                limit,
                skip
            ))
            .await?;
        if uses_ctid && result.columns.len() >= 2 {
            let keys: Vec<Vec<Option<String>>> = result
                .rows
                .iter()
                .map(|row| row.iter().take(2).cloned().collect())
                .collect();
            result.columns.drain(..2);
            for row in &mut result.rows {
                row.drain(..2);
            }
            result.keys = Some(RowKeys {
                columns: vec!["ctid".into(), "xmin".into()],
                kind: RowKeyKind::Ctid,
                values: keys,
            });
        } else if !primary_key.is_empty() {
            let indexes: Vec<usize> = primary_key
                .iter()
                .filter_map(|name| result.columns.iter().position(|c| &c.name == name))
                .collect();
            if indexes.len() == primary_key.len() {
                let values = result
                    .rows
                    .iter()
                    .map(|row| indexes.iter().map(|&i| row[i].clone()).collect())
                    .collect();
                result.keys = Some(RowKeys {
                    columns: primary_key.clone(),
                    kind: RowKeyKind::PrimaryKey,
                    values,
                });
            }
        }
        let types = self
            .query(&format!(
                "select a.attname, format_type(a.atttypid, a.atttypmod)
                 from pg_attribute a
                 where a.attrelid = {}::regclass and a.attnum > 0 and not a.attisdropped",
                literal(&self.qualified(relation))
            ))
            .await?;
        let mut by_name = std::collections::HashMap::<String, String>::new();
        for row in types.rows {
            if let (Some(Some(name)), Some(Some(kind))) = (row.first(), row.get(1)) {
                by_name.entry(name.clone()).or_insert_with(|| kind.clone());
            }
        }
        for column in &mut result.columns {
            column.type_name = by_name.get(&column.name).cloned().unwrap_or_default();
        }
        Ok(result)
    }

    /// The `where` text for a block and the rows still to skip. With the order
    /// on the key, a `hint` turns into `key > hint` (only the rows between it
    /// and `offset` are skipped); a deep skip without search first finds its
    /// key with a key-only query (an index-only scan), then starts there.
    #[allow(clippy::too_many_arguments)]
    async fn keyset(
        &self,
        relation: &Relation,
        primary_key: &[String],
        uses_ctid: bool,
        search: &str,
        sort: Option<&ColumnSort>,
        order: &str,
        offset: u64,
        hint: Option<&KeyHint>,
    ) -> DbResult<(String, u64)> {
        let searched = Self::filter(search);
        let mut conditions: Vec<String> = searched
            .strip_prefix(" where ")
            .map(|c| vec![c.to_string()])
            .unwrap_or_default();
        let joined = |conditions: &[String]| {
            if conditions.is_empty() {
                String::new()
            } else {
                format!(" where {}", conditions.join(" and "))
            }
        };
        let direction = if uses_ctid {
            sort.is_none().then_some(true)
        } else {
            keyset_direction(primary_key, sort)
        };
        let Some(ascending) = direction else {
            return Ok((joined(&conditions), offset));
        };
        let (tuple, width) = if uses_ctid {
            ("t.ctid".to_string(), 1)
        } else {
            let names: Vec<String> = primary_key.iter().map(|k| quote_ident(k)).collect();
            if names.len() == 1 {
                (names[0].clone(), 1)
            } else {
                (format!("({})", names.join(", ")), names.len())
            }
        };
        let literals = |values: &[Option<String>]| -> Option<String> {
            let parts: Option<Vec<String>> = values
                .iter()
                .take(width)
                .map(|v| v.as_deref().map(literal))
                .collect();
            let parts = parts?;
            if parts.len() != width {
                return None;
            }
            Some(if uses_ctid {
                format!("{}::tid", parts[0])
            } else if width == 1 {
                parts[0].clone()
            } else {
                format!("({})", parts.join(", "))
            })
        };
        let mut skip = offset;
        let mut bound: Option<(String, bool)> = None;
        if let Some(hint) = hint.filter(|h| h.offset < offset) {
            if let Some(value) = literals(&hint.values) {
                bound = Some((value, false));
                skip = offset - hint.offset - 1;
            }
        }
        let op = |inclusive: bool| match (ascending, inclusive) {
            (true, false) => ">",
            (true, true) => ">=",
            (false, false) => "<",
            (false, true) => "<=",
        };
        if skip >= DEEP_OFFSET && search.is_empty() && !uses_ctid {
            let mut seek = Vec::new();
            if let Some((value, _)) = &bound {
                seek.push(format!("{tuple} {} {value}", op(false)));
            }
            let found = self
                .query(&format!(
                    "select {} from {} as t{}{order} limit 1 offset {skip}",
                    primary_key
                        .iter()
                        .map(|k| quote_ident(k))
                        .collect::<Vec<_>>()
                        .join(", "),
                    self.qualified(relation),
                    joined(&seek),
                ))
                .await?;
            if let Some(value) = found.rows.first().and_then(|row| literals(row)) {
                bound = Some((value, true));
                skip = 0;
            }
        }
        if let Some((value, inclusive)) = bound {
            conditions.push(format!("{tuple} {} {value}", op(inclusive)));
        }
        Ok((joined(&conditions), skip))
    }
}

impl Drop for PostgresSession {
    fn drop(&mut self) {
        // The client going away ends the connection task by itself; this is for a stuck one.
        if let Ok(mut slot) = self.connection.lock() {
            if let Some(task) = slot.take() {
                task.abort();
            }
        }
    }
}

#[async_trait]
impl DatabaseSession for PostgresSession {
    fn engine(&self) -> Engine {
        Engine::Postgres
    }

    fn server_version(&self) -> String {
        self.server_version.clone()
    }

    /// Stops the running query. Safe to call from any thread, never waits for the session.
    /// Only the operation running when it is called is cancelled: if that one
    /// has finished by the time the request would go out, nothing is sent.
    fn cancel(&self) {
        let Some(id) = self.activity.request_cancel() else {
            return;
        };
        let (token, tls) = (self.cancel_token.clone(), self.tls.clone());
        let activity = self.activity.clone();
        self.runtime.spawn(async move {
            if activity.is_running(id) {
                let _ = token.cancel_query(tls).await;
            }
        });
    }

    fn quote(&self, identifier: &str) -> String {
        quote_ident(identifier)
    }

    /// Runs one or more statements, the way psql does with -c.
    async fn execute(&self, sql: &str) -> DbResult<Execution> {
        let _turn = self.turn().await;
        let start = Instant::now();
        let statements = sql_script::statements(sql, Dialect::Postgres);
        let outcome = self.simple(sql, &statements).await;
        let outcome = self.after_run(outcome).await;
        self.note_transaction(&statements).await;
        self.note_scope(&statements).await;
        outcome.map(|results| Execution::new(results, start.elapsed()))
    }

    async fn execute_stream(&self, sql: &str, sink: &mut StreamSink) -> DbResult<()> {
        let _turn = self.turn().await;
        let statements = sql_script::statements(sql, Dialect::Postgres);
        let outcome = self.simple_stream(sql, &statements, sink).await;
        let outcome = self.after_run(outcome).await;
        self.note_transaction(&statements).await;
        self.note_scope(&statements).await;
        outcome
    }

    fn transaction_open(&self) -> bool {
        self.in_transaction.load(Ordering::SeqCst)
    }

    fn scope_changed(&self) -> bool {
        self.scope_changed.load(Ordering::SeqCst)
    }

    fn is_closed(&self) -> bool {
        self.client.is_closed()
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
        let _turn = self.turn().await;
        self.page_in(relation, limit, offset, search, sort, hint)
            .await
    }

    /// One statement (a prepared statement refuses several, so a COMMIT can't
    /// smuggle a write in) inside a read-only transaction that is always rolled back.
    async fn read_only(&self, text: &str) -> DbResult<Execution> {
        let _turn = self.turn().await;
        let start = Instant::now();
        self.require_no_transaction().await?;
        self.command("begin transaction read only").await?;
        let outcome = async {
            self.client.prepare(text).await.map_err(|e| self.fail(e))?;
            self.one(text.to_string()).await
        }
        .await;
        self.rollback().await;
        Ok(Execution::new(vec![outcome?], start.elapsed()))
    }

    /// The script, statement by statement, in a transaction that is always rolled back.
    async fn rehearse(&self, text: &str) -> DbResult<Execution> {
        let statements = sql_script::checked(text, Dialect::Postgres)?;
        let _turn = self.turn().await;
        let start = Instant::now();
        self.require_no_transaction().await?;
        self.command("begin").await?;
        let outcome = sql_script::run(&statements, |statement| self.one(statement)).await;
        self.rollback().await;
        Ok(Execution::new(outcome?, start.elapsed()))
    }

    /// The script in one transaction: committed when every statement works, rolled back otherwise.
    async fn execute_atomically(&self, text: &str) -> DbResult<Execution> {
        let statements = sql_script::checked(text, Dialect::Postgres)?;
        let _turn = self.turn().await;
        let start = Instant::now();
        self.require_no_transaction().await?;
        self.command("begin").await?;
        let outcome = sql_script::run(&statements, |statement| self.one(statement)).await;
        match outcome {
            Ok(results) => match self.command("commit").await {
                Ok(()) => {
                    self.note_scope(&statements).await;
                    Ok(Execution::new(results, start.elapsed()))
                }
                Err(error) => {
                    self.rollback().await;
                    Err(error)
                }
            },
            Err(error) => {
                self.rollback().await;
                Err(error)
            }
        }
    }

    async fn relations(&self) -> DbResult<Vec<Relation>> {
        let _turn = self.turn().await;
        let result = self
            .query(
                "select n.nspname, c.relname, c.relkind, c.reltuples
                 from pg_class c join pg_namespace n on n.oid = c.relnamespace
                 where c.relkind in ('r','p','v','m','f')
                   and n.nspname not in ('pg_catalog','information_schema')
                   and n.nspname not like 'pg_toast%' and n.nspname not like 'pg_temp%'
                 order by n.nspname = 'public' desc, n.nspname, c.relname",
            )
            .await?;
        Ok(result
            .rows
            .into_iter()
            .map(|row| {
                let text = |i: usize| row.get(i).cloned().flatten();
                Relation::new(
                    text(0).unwrap_or_default(),
                    text(1).unwrap_or_default(),
                    relation_kind(text(2).as_deref().unwrap_or("")),
                    text(3).and_then(|e| e.parse::<f64>().ok()).unwrap_or(-1.0),
                )
            })
            .collect())
    }

    /// One page of a table's rows, with the column types and, for tables, the row
    /// keys. Sorted by the primary key when there is one, so pages and edited rows
    /// stay in place.
    async fn page(
        &self,
        relation: &Relation,
        limit: u32,
        offset: u64,
        search: &str,
        sort: Option<&ColumnSort>,
    ) -> DbResult<QueryResult> {
        let _turn = self.turn().await;
        self.page_in(relation, limit, offset, search, sort, None)
            .await
    }

    /// Exact row count, or None when the table is big enough that counting is slow.
    async fn count(&self, relation: &Relation, search: &str) -> DbResult<Option<u64>> {
        if relation.estimate > 500_000.0 {
            return Ok(None);
        }
        let _turn = self.turn().await;
        let result = self
            .query(&format!(
                "select count(*) from {} as t{}",
                self.qualified(relation),
                Self::filter(search)
            ))
            .await?;
        Ok(result
            .rows
            .first()
            .and_then(|row| row.first().cloned().flatten())
            .and_then(|n| n.parse().ok()))
    }

    /// Changes one row. Postgres casts each text value to its column's type.
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
        let _turn = self.quiet_turn().await;
        let mut params: Vec<Option<String>> = changes.iter().map(|c| c.value.clone()).collect();
        let set: Vec<String> = changes
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{} = ${}", quote_ident(&c.column), i + 1))
            .collect();
        let (filter, key_params) = key_filter(key, params.len())?;
        params.extend(key_params);
        let sql = format!(
            "update {}{} set {} where {}",
            only(relation),
            self.qualified(relation),
            set.join(", "),
            filter
        );
        self.edit(sql, params, true, |n| format!("UPDATE {n}"))
            .await
    }

    /// Adds a row. Columns left out get their default.
    async fn insert(&self, relation: &Relation, values: &[CellValue]) -> DbResult<EditOutcome> {
        let _turn = self.quiet_turn().await;
        let sql = if values.is_empty() {
            format!("insert into {} default values", self.qualified(relation))
        } else {
            format!(
                "insert into {} ({}) values ({})",
                self.qualified(relation),
                values
                    .iter()
                    .map(|v| quote_ident(&v.column))
                    .collect::<Vec<_>>()
                    .join(", "),
                (1..=values.len())
                    .map(|i| format!("${i}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        self.edit(
            sql,
            values.iter().map(|v| v.value.clone()).collect(),
            false,
            |n| format!("INSERT 0 {n}"),
        )
        .await
    }

    async fn delete(&self, relation: &Relation, key: &RowKey) -> DbResult<EditOutcome> {
        let _turn = self.quiet_turn().await;
        let (filter, params) = key_filter(key, 0)?;
        let sql = format!(
            "delete from {}{} where {}",
            only(relation),
            self.qualified(relation),
            filter
        );
        self.edit(sql, params, true, |n| format!("DELETE {n}"))
            .await
    }

    async fn schema_graph(&self) -> DbResult<SchemaGraph> {
        let _turn = self.turn().await;
        let columns = self
            .query(
                "select n.nspname, c.relname, c.relkind in ('v','m'), a.attname,
                        format_type(a.atttypid, a.atttypmod), not a.attnotnull,
                        coalesce(a.attnum = any(i.indkey::int2[]), false)
                 from pg_class c
                 join pg_namespace n on n.oid = c.relnamespace
                 join pg_attribute a on a.attrelid = c.oid and a.attnum > 0 and not a.attisdropped
                 left join pg_index i on i.indrelid = c.oid and i.indisprimary
                 where c.relkind in ('r','p','v','m','f') and not c.relispartition
                   and n.nspname not in ('pg_catalog','information_schema')
                   and n.nspname not like 'pg_toast%' and n.nspname not like 'pg_temp%'
                 order by n.nspname = 'public' desc, n.nspname, c.relname, a.attnum",
            )
            .await?
            .rows;
        let links = self
            .query(
                "select con.conname, sn.nspname || '.' || sc.relname, tn.nspname || '.' || tc.relname,
                        (select string_agg(a.attname, chr(31) order by k.n) from unnest(con.conkey) with ordinality k(attnum, n)
                           join pg_attribute a on a.attrelid = con.conrelid and a.attnum = k.attnum),
                        (select string_agg(a.attname, chr(31) order by k.n) from unnest(con.confkey) with ordinality k(attnum, n)
                           join pg_attribute a on a.attrelid = con.confrelid and a.attnum = k.attnum)
                 from pg_constraint con
                 join pg_class sc on sc.oid = con.conrelid join pg_namespace sn on sn.oid = sc.relnamespace
                 join pg_class tc on tc.oid = con.confrelid join pg_namespace tn on tn.oid = tc.relnamespace
                 where con.contype = 'f' and sn.nspname not in ('pg_catalog','information_schema')
                 order by 1",
            )
            .await?
            .rows;
        Ok(SchemaGraph::build(&columns, &links))
    }

    async fn close(&self) {
        let task = self.connection.lock().ok().and_then(|mut slot| slot.take());
        if let Some(task) = task {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(text: &str) -> ConnectionUrl {
        ConnectionUrl::new(text).expect("url")
    }

    #[test]
    fn literals_escape_quotes_and_backslashes() {
        assert_eq!(literal("it's"), "'it''s'");
        assert_eq!(literal("a\\b'c"), "E'a\\\\b''c'");
    }

    #[test]
    fn versions_print_like_swift() {
        assert_eq!(format_version(160004), "16.4");
        assert_eq!(format_version(150013), "15.13");
    }

    #[test]
    fn tags_follow_the_statement() {
        assert_eq!(command_tag("select 1", 3, true), "SELECT 3");
        assert_eq!(
            command_tag("insert into t values (1)", 1, false),
            "INSERT 0 1"
        );
        assert_eq!(
            command_tag("-- c\nupdate t set a = 1", 2, false),
            "UPDATE 2"
        );
        assert_eq!(
            command_tag("create table t(a int)", 0, false),
            "CREATE TABLE"
        );
        assert_eq!(
            command_tag("create or replace temp view v as select 1", 0, false),
            "CREATE VIEW"
        );
        assert_eq!(
            command_tag("drop table if exists t", 0, false),
            "DROP TABLE"
        );
        assert_eq!(
            command_tag("create unique index concurrently i on t(a)", 0, false),
            "CREATE INDEX"
        );
        assert_eq!(
            command_tag("create materialized view m as select 1", 0, false),
            "CREATE MATERIALIZED VIEW"
        );
        assert_eq!(command_tag("truncate t", 0, false), "TRUNCATE TABLE");
        assert_eq!(command_tag("set search_path = public", 0, false), "SET");
        assert_eq!(
            command_tag("with x as (select 1) select * from x", 1, true),
            "SELECT 1"
        );
        assert_eq!(
            command_tag(
                "with x as (select 1) insert into t select * from x",
                1,
                false
            ),
            "INSERT 0 1"
        );
        assert_eq!(
            command_tag(
                "with d as (delete from t returning *) select * from d",
                2,
                true
            ),
            "DELETE 2"
        );
        assert_eq!(command_tag("", 0, false), "");
    }

    #[test]
    fn relation_kinds() {
        assert_eq!(relation_kind("r"), RelationKind::Table);
        assert_eq!(relation_kind("p"), RelationKind::Partitioned);
        assert_eq!(relation_kind("v"), RelationKind::View);
        assert_eq!(relation_kind("m"), RelationKind::Materialized);
        assert_eq!(relation_kind("f"), RelationKind::Foreign);
    }

    fn key(kind: RowKeyKind, columns: &[&str], values: &[Option<&str>]) -> RowKey {
        RowKey {
            columns: columns.iter().map(|c| c.to_string()).collect(),
            kind,
            values: values.iter().map(|v| v.map(String::from)).collect(),
        }
    }

    #[test]
    fn key_filters_number_their_parameters() {
        let (sql, params) = key_filter(
            &key(
                RowKeyKind::PrimaryKey,
                &["a", "b\"c"],
                &[Some("1"), Some("2")],
            ),
            2,
        )
        .unwrap();
        assert_eq!(sql, "\"a\" = $3 and \"b\"\"c\" = $4");
        assert_eq!(params, vec![Some("1".to_string()), Some("2".to_string())]);
        let (sql, _) = key_filter(
            &key(
                RowKeyKind::Ctid,
                &["ctid", "xmin"],
                &[Some("(0,1)"), Some("7")],
            ),
            0,
        )
        .unwrap();
        assert_eq!(sql, "ctid = $1::tid and xmin::text = $2");
        assert!(key_filter(&key(RowKeyKind::Rowid, &["rowid"], &[Some("1")]), 0).is_err());
        assert!(key_filter(&key(RowKeyKind::PrimaryKey, &["a"], &[]), 0).is_err());
    }

    #[test]
    fn only_guards_plain_tables() {
        let table = Relation::new("public", "t", RelationKind::Table, 1.0);
        let part = Relation::new("public", "p", RelationKind::Partitioned, 1.0);
        assert_eq!(only(&table), "only ");
        assert_eq!(only(&part), "");
    }

    #[test]
    fn ssl_modes_parse() {
        assert_eq!(Ssl::parse("verify-full").unwrap(), Ssl::VerifyFull);
        assert_eq!(Ssl::parse("DISABLE").unwrap(), Ssl::Disable);
        assert!(Ssl::parse("sometimes").is_err());
    }

    #[test]
    fn config_follows_the_url() {
        let url = parsed("postgres://ana:s3cret@db.example.com:6543/loja?sslmode=verify-ca&sslrootcert=/x/root.crt");
        let (config, spec) = build_config(&url, None).unwrap();
        assert_eq!(config.get_user(), Some("ana"));
        assert_eq!(config.get_password(), Some(&b"s3cret"[..]));
        assert_eq!(config.get_dbname(), Some("loja"));
        assert_eq!(config.get_ports(), &[6543]);
        assert_eq!(
            spec,
            TlsSpec {
                mode: Ssl::VerifyCa,
                root_cert: Some("/x/root.crt".into())
            }
        );
        assert_eq!(config.get_ssl_mode(), PgSslMode::Require);
        assert_eq!(config.get_application_name(), Some("dbLens"));
    }

    #[test]
    fn default_mode_prefers_tls_and_disable_is_plain() {
        let (config, spec) = build_config(&parsed("postgres://u@h/db"), None).unwrap();
        assert_eq!(spec.mode, Ssl::Prefer);
        assert_eq!(config.get_ssl_mode(), PgSslMode::Prefer);
        let (config, _) = build_config(&parsed("postgres://u@h/db?sslmode=disable"), None).unwrap();
        assert_eq!(config.get_ssl_mode(), PgSslMode::Disable);
        assert!(build_config(&parsed("postgres://u@h/db?sslmode=bogus"), None).is_err());
    }

    #[test]
    fn tunnels_keep_the_host_name_for_tls() {
        let (config, _) =
            build_config(&parsed("postgres://u@db.example.com:5432/db"), Some(40123)).unwrap();
        assert_eq!(config.get_hosts().len(), 1);
        assert_eq!(
            config.get_hostaddrs(),
            &["127.0.0.1".parse::<std::net::IpAddr>().unwrap()]
        );
        assert_eq!(config.get_ports(), &[40123]);
        assert!(
            matches!(&config.get_hosts()[0], tokio_postgres::config::Host::Tcp(h) if h == "db.example.com")
        );
    }

    #[test]
    fn several_hosts_are_all_tried() {
        let (config, _) = build_config(&parsed("postgresql://u@a:5432,b:5433/db"), None).unwrap();
        assert_eq!(config.get_hosts().len(), 2);
    }

    #[test]
    fn tls_configs_build_for_every_mode() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        for mode in [
            Ssl::Disable,
            Ssl::Allow,
            Ssl::Prefer,
            Ssl::Require,
            Ssl::VerifyCa,
            Ssl::VerifyFull,
        ] {
            make_tls(&TlsSpec {
                mode,
                root_cert: None,
            })
            .unwrap_or_else(|e| panic!("{mode:?}: {e}"));
        }
        assert!(make_tls(&TlsSpec {
            mode: Ssl::VerifyFull,
            root_cert: Some("/no/such/root.crt".into())
        })
        .is_err());
    }

    #[test]
    fn summary_reads_the_url() {
        let s = summary("postgres://ana@db.example.com:5433/loja?sslmode=require").unwrap();
        assert_eq!(s.database, "loja");
        assert_eq!(s.port, Some(5433));
        assert_eq!(s.tls.as_deref(), Some("require"));
        assert!(summary("not a url").is_none());
    }

    #[test]
    fn text_params_encode_as_text() {
        let mut out = BytesMut::new();
        assert!(matches!(
            TextParam(Some("héllo".into()))
                .to_sql(&Type::INT4, &mut out)
                .unwrap(),
            IsNull::No
        ));
        assert_eq!(&out[..], "héllo".as_bytes());
        assert!(matches!(
            TextParam(None).to_sql(&Type::TEXT, &mut out).unwrap(),
            IsNull::Yes
        ));
        assert!(matches!(
            TextParam(None).encode_format(&Type::INT4),
            Format::Text
        ));
    }
}
