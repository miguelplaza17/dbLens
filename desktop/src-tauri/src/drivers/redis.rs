//! Redis / Valkey driver. Port of `Sources/DBLensKit/Redis.swift`.
//!
//! Own RESP client over tokio (TCP, or TLS with rustls/ring): the keys of the
//! database are the relations (found with SCAN, never KEYS, grouped by the
//! prefix before the first ":"), a page shows what one key holds by type, the
//! command panel runs redis-cli style lines. There is no transaction to roll
//! back, so `rehearse` refuses and `execute_atomically` is `NotAtomic` (trait
//! defaults). `cancel` closes the socket of the running call; the next call
//! reconnects.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures_util::future::BoxFuture;
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
};
use tokio::net::TcpStream;
use tokio::sync::{watch, Mutex};

use crate::connector::Summary;
use crate::engine::Engine;
use crate::session::{
    CellValue, Column, ColumnSort, DatabaseSession, DbError, DbResult, EditOutcome, Execution,
    QueryResult, Relation, RelationKind, RowKey, RowKeyKind, RowKeys, ROW_LIMIT,
};
use crate::url::UrlParts;

/// The most keys `relations()` lists.
pub const KEY_LIMIT: usize = 5000;
/// Bytes shown of a value that isn't text.
const HEX_LIMIT: usize = 64;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

type Cell = Option<String>;
type Row = Vec<Cell>;

/// Host, database index, user and TLS for the connection sheet.
pub fn summary(text: &str) -> Option<Summary> {
    let parts = UrlParts::new(text)
        .filter(|p| ["redis", "rediss", "valkey", "valkeys"].contains(&p.scheme.as_str()))?;
    let host = parts
        .first_host()
        .map(|h| h.name.clone())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "localhost".into());
    let index: u32 = parts
        .path
        .parse()
        .ok()
        .or_else(|| parts.query.get("db").and_then(|d| d.parse().ok()))
        .unwrap_or(0);
    Some(Summary {
        engine: Engine::Redis,
        database: format!("db {index}"),
        suggested_name: format!("db {index} @ {host}"),
        host: Some(host),
        port: parts.first_host().and_then(|h| h.port),
        user: parts.user.clone().filter(|u| !u.is_empty()),
        tls: matches!(parts.scheme.as_str(), "rediss" | "valkeys").then(|| "TLS".to_string()),
    })
}

// MARK: - URL

/// A Redis (or Valkey) connection, as pasted.
///   redis://:pass@cache.example.com:6379/0     (password only)
///   redis://ana:pass@cache.example.com/3       (ACL user)
///   rediss://...                               (TLS)
///   rediss://...?ssl_cert_reqs=none            (TLS, certificate not checked)
///   redis://host?db=3&password=...
/// Sentinel and Cluster URLs are refused for now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedisUrl {
    pub host: String,
    pub port: u16,
    /// The ACL user; None for `redis://:pass@host`.
    pub user: Option<String>,
    pub password: Option<String>,
    pub database: u32,
    pub tls: bool,
    /// False when the URL says not to check the server's certificate.
    pub verify_tls: bool,
}

impl RedisUrl {
    pub fn parse(text: &str) -> DbResult<Self> {
        let cleaned = crate::url::clean(text);
        if cleaned.is_empty() {
            return Err(DbError::msg("Paste the database URL."));
        }
        let lower = cleaned.to_lowercase();
        if let Some((scheme, _)) = lower.split_once("://") {
            for base in ["redis", "rediss", "valkey", "valkeys"] {
                if let Some(rest) = scheme.strip_prefix(base) {
                    let mut chars = rest.chars();
                    if matches!(chars.next(), Some('+' | '-')) && !rest[1..].is_empty() {
                        return Err(DbError::msg(
                            "Redis Sentinel, Cluster and socket URLs are not supported yet. Use a redis:// URL to one server.",
                        ));
                    }
                }
            }
        }
        let parts = UrlParts::new(&cleaned)
            .filter(|p| ["redis", "rediss", "valkey", "valkeys"].contains(&p.scheme.as_str()))
            .ok_or_else(|| {
                DbError::msg("Unrecognized format. Use a redis://… URL (or rediss:// for TLS)")
            })?;
        let query: HashMap<String, String> = parts
            .query
            .iter()
            .map(|(k, v)| (k.to_lowercase(), v.clone()))
            .collect();
        let non_empty = |v: Option<&String>| v.filter(|s| !s.is_empty()).cloned();
        let flag = |name: &str| -> Option<bool> {
            match query.get(name)?.to_lowercase().as_str() {
                "true" | "1" | "yes" | "on" => Some(true),
                "false" | "0" | "no" | "off" => Some(false),
                _ => None,
            }
        };
        let number = non_empty(Some(&parts.path))
            .or_else(|| non_empty(query.get("db").or_else(|| query.get("database"))));
        let database = match number {
            None => 0,
            Some(n) => n
                .parse::<u32>()
                .map_err(|_| DbError::msg("The Redis database must be a number, like /0."))?,
        };
        let host =
            non_empty(parts.first_host().map(|h| &h.name)).unwrap_or_else(|| "localhost".into());
        let skip = query.get("ssl_cert_reqs").map(|v| v.to_lowercase()) == Some("none".into())
            || flag("insecure") == Some(true)
            || flag("skip_verify") == Some(true)
            || flag("tls_skip_verify") == Some(true);
        Ok(Self {
            host,
            port: parts.first_host().and_then(|h| h.port).unwrap_or(6379),
            user: non_empty(parts.user.as_ref())
                .or_else(|| non_empty(query.get("username").or_else(|| query.get("user")))),
            password: non_empty(parts.password.as_ref())
                .or_else(|| non_empty(query.get("password"))),
            database,
            tls: matches!(parts.scheme.as_str(), "rediss" | "valkeys")
                || flag("ssl") == Some(true)
                || flag("tls") == Some(true),
            verify_tls: !skip,
        })
    }
}

// MARK: - RESP

/// One reply in the Redis protocol (RESP2, plus the RESP3 types a server may send).
#[derive(Debug, Clone, PartialEq)]
pub enum Resp {
    Simple(String),
    Error(String),
    Integer(i64),
    Bulk(Vec<u8>),
    Null,
    Array(Vec<Resp>),
    /// A RESP3 map, flattened: key, value, key, value...
    Map(Vec<Resp>),
    Double(String),
    Boolean(bool),
    BigNumber(String),
}

impl Resp {
    /// The reply as one cell: text, 0x... for bytes that aren't UTF-8, None for
    /// null, JSON-ish text for arrays and maps.
    pub fn text(&self) -> Option<String> {
        match self {
            Resp::Simple(t) | Resp::Error(t) | Resp::Double(t) | Resp::BigNumber(t) => {
                Some(t.clone())
            }
            Resp::Integer(n) => Some(n.to_string()),
            Resp::Boolean(b) => Some(b.to_string()),
            Resp::Bulk(bytes) => Some(display(bytes)),
            Resp::Null => None,
            Resp::Array(_) | Resp::Map(_) => Some(jsonish(self)),
        }
    }

    pub fn integer(&self) -> Option<i64> {
        match self {
            Resp::Integer(n) => Some(*n),
            Resp::Bulk(_) | Resp::Simple(_) => self.text().and_then(|t| t.parse().ok()),
            _ => None,
        }
    }

    /// The bytes of a string reply.
    pub fn bytes(&self) -> Option<Vec<u8>> {
        match self {
            Resp::Bulk(b) => Some(b.clone()),
            Resp::Simple(t) => Some(t.as_bytes().to_vec()),
            _ => None,
        }
    }

    /// A bulk string that isn't UTF-8, shown as hex.
    pub fn is_binary(&self) -> bool {
        matches!(self, Resp::Bulk(b) if std::str::from_utf8(b).is_err())
    }

    pub fn is_aggregate(&self) -> bool {
        matches!(self, Resp::Array(_) | Resp::Map(_))
    }
}

/// UTF-8 text, or 0x... cut to 64 bytes when the bytes aren't text.
pub fn display(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(_) => {
            let mut out = String::from("0x");
            for b in bytes.iter().take(HEX_LIMIT) {
                out.push_str(&format!("{b:02X}"));
            }
            if bytes.len() > HEX_LIMIT {
                out.push('…');
            }
            out
        }
    }
}

/// Arrays and maps as JSON-ish text, for one cell.
pub fn jsonish(value: &Resp) -> String {
    match value {
        Resp::Array(items) => format!(
            "[{}]",
            items.iter().map(jsonish).collect::<Vec<_>>().join(", ")
        ),
        Resp::Map(flat) => {
            let pairs: Vec<String> = flat
                .chunks(2)
                .filter(|c| c.len() == 2)
                .map(|c| format!("{}: {}", jsonish(&c[0]), jsonish(&c[1])))
                .collect();
            format!("{{{}}}", pairs.join(", "))
        }
        Resp::Null => "null".into(),
        Resp::Integer(_) | Resp::Double(_) | Resp::BigNumber(_) | Resp::Boolean(_) => {
            value.text().unwrap_or_default()
        }
        Resp::Error(m) => quoted(&format!("(error) {m}")),
        Resp::Simple(_) | Resp::Bulk(_) => quoted(&value.text().unwrap_or_default()),
    }
}

fn quoted(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The commands as RESP arrays of bulk strings, ready to send in one write.
pub fn encode(commands: &[Vec<Vec<u8>>]) -> Vec<u8> {
    let mut data = Vec::new();
    for command in commands {
        data.extend_from_slice(format!("*{}\r\n", command.len()).as_bytes());
        for arg in command {
            data.extend_from_slice(format!("${}\r\n", arg.len()).as_bytes());
            data.extend_from_slice(arg);
            data.extend_from_slice(b"\r\n");
        }
    }
    data
}

fn io_message(error: &std::io::Error) -> String {
    if error.kind() == std::io::ErrorKind::UnexpectedEof {
        "The server closed the connection.".into()
    } else {
        error.to_string()
    }
}

fn bad(what: &str) -> String {
    format!("The server sent a reply dbLens doesn't understand ({what}).")
}

async fn read_line<R: AsyncBufRead + Unpin>(io: &mut R) -> Result<String, String> {
    let mut line = Vec::new();
    let n = io
        .read_until(b'\n', &mut line)
        .await
        .map_err(|e| io_message(&e))?;
    if n == 0 {
        return Err("The server closed the connection.".into());
    }
    line.pop();
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    Ok(String::from_utf8_lossy(&line).into_owned())
}

async fn read_bytes<R: AsyncBufRead + Unpin>(io: &mut R, length: i64) -> Result<Vec<u8>, String> {
    let length = usize::try_from(length).map_err(|_| bad("bad length"))?;
    let mut buffer = vec![0u8; length + 2];
    io.read_exact(&mut buffer)
        .await
        .map_err(|e| io_message(&e))?;
    if &buffer[length..] != b"\r\n" {
        return Err(bad("missing CRLF"));
    }
    buffer.truncate(length);
    Ok(buffer)
}

/// The next reply from a byte stream.
pub fn read_resp<R: AsyncBufRead + Unpin + Send>(
    io: &mut R,
) -> BoxFuture<'_, Result<Resp, String>> {
    Box::pin(async move {
        let line = read_line(io).await?;
        let mut chars = line.chars();
        let Some(marker) = chars.next() else {
            return Err(bad("empty line"));
        };
        let rest = chars.as_str().to_string();
        let count = |text: &str| {
            text.parse::<i64>()
                .map_err(|_| bad(&format!("bad length {text}")))
        };
        match marker {
            '+' => Ok(Resp::Simple(rest)),
            '-' => Ok(Resp::Error(rest)),
            ':' => rest
                .parse()
                .map(Resp::Integer)
                .map_err(|_| bad(&format!("bad integer {rest}"))),
            '$' => {
                let n = count(&rest)?;
                if n < 0 {
                    Ok(Resp::Null)
                } else {
                    Ok(Resp::Bulk(read_bytes(io, n).await?))
                }
            }
            '*' | '~' | '>' => {
                let n = count(&rest)?;
                if n < 0 {
                    return Ok(Resp::Null);
                }
                let mut items = Vec::with_capacity((n as usize).min(1024));
                for _ in 0..n {
                    items.push(read_resp(io).await?);
                }
                Ok(Resp::Array(items))
            }
            '%' => {
                let n = count(&rest)?.max(0);
                let mut items = Vec::with_capacity(((2 * n) as usize).min(1024));
                for _ in 0..2 * n {
                    items.push(read_resp(io).await?);
                }
                Ok(Resp::Map(items))
            }
            '_' => Ok(Resp::Null),
            '#' => Ok(Resp::Boolean(rest == "t")),
            ',' => Ok(Resp::Double(rest)),
            '(' => Ok(Resp::BigNumber(rest)),
            '!' => {
                let n = count(&rest)?;
                Ok(Resp::Error(
                    String::from_utf8_lossy(&read_bytes(io, n).await?).into_owned(),
                ))
            }
            '=' => {
                // Verbatim string: "txt:" or "mkd:" then the text.
                let n = count(&rest)?;
                let bytes = read_bytes(io, n).await?;
                Ok(Resp::Bulk(bytes.get(4..).unwrap_or_default().to_vec()))
            }
            '|' => {
                // Attributes come before the reply they describe; skip them.
                for _ in 0..2 * count(&rest)?.max(0) {
                    read_resp(io).await?;
                }
                read_resp(io).await
            }
            other => Err(bad(&format!("unexpected reply type {other}"))),
        }
    })
}

// MARK: - Socket

trait Duplex: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Duplex for T {}

/// One TCP (or TLS) connection to the server.
struct Link {
    io: BufReader<Box<dyn Duplex>>,
}

impl Link {
    async fn send(&mut self, commands: &[Vec<Vec<u8>>]) -> Result<(), String> {
        let data = encode(commands);
        let out = self.io.get_mut();
        out.write_all(&data).await.map_err(|e| io_message(&e))?;
        out.flush().await.map_err(|e| io_message(&e))
    }

    async fn read(&mut self) -> Result<Resp, String> {
        read_resp(&mut self.io).await
    }
}

mod tls {
    use std::sync::Arc;

    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerifier};
    use rustls::crypto::{ring, CryptoProvider};
    use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
    use rustls::{ClientConfig, DigitallySignedStruct, Error, RootCertStore, SignatureScheme};

    /// Accepts any certificate (`?ssl_cert_reqs=none`); signatures are still checked.
    #[derive(Debug)]
    struct NoVerify(Arc<CryptoProvider>);

    impl ServerCertVerifier for NoVerify {
        fn verify_server_cert(
            &self,
            _end_entity: &CertificateDer<'_>,
            _intermediates: &[CertificateDer<'_>],
            _server_name: &ServerName<'_>,
            _ocsp: &[u8],
            _now: UnixTime,
        ) -> Result<rustls::client::danger::ServerCertVerified, Error> {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, Error> {
            rustls::crypto::verify_tls12_signature(
                message,
                cert,
                dss,
                &self.0.signature_verification_algorithms,
            )
        }

        fn verify_tls13_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, Error> {
            rustls::crypto::verify_tls13_signature(
                message,
                cert,
                dss,
                &self.0.signature_verification_algorithms,
            )
        }

        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            self.0.signature_verification_algorithms.supported_schemes()
        }
    }

    pub fn config(verify: bool) -> Result<Arc<ClientConfig>, Error> {
        let provider = Arc::new(ring::default_provider());
        let builder = ClientConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()?;
        let config = if verify {
            let mut roots = RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            builder.with_root_certificates(roots).with_no_client_auth()
        } else {
            builder
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(NoVerify(provider)))
                .with_no_client_auth()
        };
        Ok(Arc::new(config))
    }
}

/// Where to connect and how to log in; reused to reconnect after a cancel.
#[derive(Clone)]
struct Target {
    host: String,
    port: u16,
    server_name: String,
    url: RedisUrl,
}

impl Target {
    fn new(url: RedisUrl, local_port: Option<u16>) -> Self {
        Self {
            host: if local_port.is_some() {
                "127.0.0.1".into()
            } else {
                url.host.clone()
            },
            port: local_port.unwrap_or(url.port),
            server_name: url.host.clone(),
            url,
        }
    }

    async fn socket(&self) -> DbResult<Box<dyn Duplex>> {
        let (host, port) = (&self.host, self.port);
        let fail =
            |reason: String| DbError::msg(format!("Couldn't connect to {host}:{port}: {reason}"));
        let tcp =
            match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((host.as_str(), port)))
                .await
            {
                Err(_) => {
                    return Err(DbError::msg(format!(
                        "Couldn't connect to {host}:{port} within {} seconds.",
                        CONNECT_TIMEOUT.as_secs()
                    )))
                }
                Ok(Err(e)) => return Err(fail(e.to_string())),
                Ok(Ok(tcp)) => tcp,
            };
        let _ = tcp.set_nodelay(true);
        if !self.url.tls {
            return Ok(Box::new(tcp));
        }
        let config =
            tls::config(self.url.verify_tls).map_err(|e| fail(format!("TLS failed: {e}")))?;
        let name =
            rustls::pki_types::ServerName::try_from(self.server_name.clone()).map_err(|_| {
                fail(format!(
                    "TLS failed: {} is not a valid server name",
                    self.server_name
                ))
            })?;
        let stream = tokio::time::timeout(
            CONNECT_TIMEOUT,
            tokio_rustls::TlsConnector::from(config).connect(name, tcp),
        )
        .await
        .map_err(|_| fail("TLS handshake timed out".into()))?
        .map_err(|e| fail(format!("TLS failed: {e}")))?;
        Ok(Box::new(stream))
    }

    /// Connects, logs in, selects the database and pings, or gives the server's message.
    async fn open(&self, database: u32) -> DbResult<Link> {
        let mut link = Link {
            io: BufReader::new(self.socket().await?),
        };
        let mut commands = Vec::new();
        if let Some(password) = &self.url.password {
            let mut auth = vec!["AUTH"];
            if let Some(user) = &self.url.user {
                auth.push(user);
            }
            auth.push(password);
            commands.push(args(&auth));
        }
        if database != 0 {
            commands.push(args(&["SELECT", &database.to_string()]));
        }
        commands.push(args(&["PING"]));
        link.send(&commands).await.map_err(DbError::Message)?;
        let mut failure = None;
        for _ in &commands {
            if let Resp::Error(message) = link.read().await.map_err(DbError::Message)? {
                failure.get_or_insert(message);
            }
        }
        match failure {
            Some(message) => Err(DbError::Message(message)),
            None => Ok(link),
        }
    }
}

fn args(parts: &[&str]) -> Vec<Vec<u8>> {
    parts.iter().map(|p| p.as_bytes().to_vec()).collect()
}

// MARK: - The command panel: text to commands, replies to grids

/// Commands that never return, so the panel would wait forever.
const STREAMING: [&str; 6] = [
    "SUBSCRIBE",
    "PSUBSCRIBE",
    "SSUBSCRIBE",
    "MONITOR",
    "SYNC",
    "PSYNC",
];

/// Commands that only read, for agents. Redis has no read-only transaction, so
/// anything else is refused before it reaches the server.
pub const READING_COMMANDS: &[&str] = &[
    "GET",
    "MGET",
    "GETRANGE",
    "STRLEN",
    "EXISTS",
    "TYPE",
    "TTL",
    "PTTL",
    "EXPIRETIME",
    "KEYS",
    "SCAN",
    "DBSIZE",
    "HGET",
    "HMGET",
    "HGETALL",
    "HKEYS",
    "HVALS",
    "HLEN",
    "HEXISTS",
    "HSTRLEN",
    "HSCAN",
    "HRANDFIELD",
    "LRANGE",
    "LLEN",
    "LINDEX",
    "LPOS",
    "SMEMBERS",
    "SCARD",
    "SISMEMBER",
    "SMISMEMBER",
    "SRANDMEMBER",
    "SSCAN",
    "SINTER",
    "SUNION",
    "SDIFF",
    "SINTERCARD",
    "ZRANGE",
    "ZRANGEBYSCORE",
    "ZRANGEBYLEX",
    "ZREVRANGE",
    "ZREVRANGEBYSCORE",
    "ZCARD",
    "ZSCORE",
    "ZMSCORE",
    "ZRANK",
    "ZREVRANK",
    "ZCOUNT",
    "ZLEXCOUNT",
    "ZSCAN",
    "ZRANDMEMBER",
    "XRANGE",
    "XREVRANGE",
    "XLEN",
    "XINFO",
    "PING",
    "ECHO",
    "TIME",
    "INFO",
    "JSON.GET",
    "JSON.MGET",
    "JSON.TYPE",
    "JSON.STRLEN",
    "JSON.ARRLEN",
    "JSON.OBJKEYS",
    "JSON.OBJLEN",
];

/// Every command line of the text, split into arguments. Blank lines and lines
/// starting with # are skipped.
pub fn commands_in(text: &str) -> DbResult<Vec<Vec<Vec<u8>>>> {
    let mut out = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_matches(|c| c == ' ' || c == '\t');
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let parts = tokenize(trimmed)?;
        if !parts.is_empty() {
            out.push(parts);
        }
    }
    Ok(out)
}

/// Splits a line like redis-cli: spaces between arguments; "double quotes" with
/// \" \\ \n \r \t \b \a and \xHH escapes; 'single quotes' taken as written (only
/// \' escapes a quote). A quote must end its argument.
pub fn tokenize(line: &str) -> DbResult<Vec<Vec<u8>>> {
    let bytes = line.as_bytes();
    let unbalanced = || DbError::msg(format!("Unbalanced quotes in: {line}"));
    let is_space = |b: u8| matches!(b, 9..=13 | 32);
    let hex = |b: u8| match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'A'..=b'F' => Some(b - b'A' + 10),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    };
    let mut parts = Vec::new();
    let mut i = 0;
    loop {
        while i < bytes.len() && is_space(bytes[i]) {
            i += 1;
        }
        if i >= bytes.len() {
            return Ok(parts);
        }
        let mut current: Vec<u8> = Vec::new();
        let mut quote: Option<u8> = None;
        while i < bytes.len() {
            let byte = bytes[i];
            if let Some(open) = quote {
                if open == b'"' && byte == b'\\' && i + 1 < bytes.len() {
                    let next = bytes[i + 1];
                    if next == b'x' && i + 3 < bytes.len() {
                        if let (Some(high), Some(low)) = (hex(bytes[i + 2]), hex(bytes[i + 3])) {
                            current.push(high << 4 | low);
                            i += 4;
                            continue;
                        }
                    }
                    current.push(match next {
                        b'n' => 10,
                        b'r' => 13,
                        b't' => 9,
                        b'b' => 8,
                        b'a' => 7,
                        other => other,
                    });
                    i += 2;
                } else if open == b'\'' && byte == b'\\' && bytes.get(i + 1) == Some(&b'\'') {
                    current.push(b'\'');
                    i += 2;
                } else if byte == open {
                    i += 1;
                    // The closing quote must be followed by a space or the end.
                    if i < bytes.len() && !is_space(bytes[i]) {
                        return Err(unbalanced());
                    }
                    quote = None;
                    break;
                } else {
                    current.push(byte);
                    i += 1;
                }
            } else if is_space(byte) {
                break;
            } else if (byte == b'"' || byte == b'\'') && current.is_empty() {
                quote = Some(byte);
                i += 1;
            } else {
                current.push(byte);
                i += 1;
            }
        }
        if quote.is_some() {
            return Err(unbalanced());
        }
        parts.push(current);
    }
}

/// The command's name, upper-case.
pub fn name_of(parts: &[Vec<u8>]) -> String {
    parts
        .first()
        .map(|p| String::from_utf8_lossy(p).to_uppercase())
        .unwrap_or_default()
}

/// Commands whose flat replies alternate two things, and what to call them.
fn pair_names(parts: &[Vec<u8>]) -> Option<(&'static str, &'static str)> {
    let words: Vec<String> = parts
        .iter()
        .map(|p| String::from_utf8_lossy(p).to_uppercase())
        .collect();
    let name = words.first().map(String::as_str).unwrap_or("");
    let sub = words.get(1).map(String::as_str).unwrap_or("");
    let with = |w: &str| words.iter().any(|x| x == w);
    match name {
        "HGETALL" => Some(("field", "value")),
        "CONFIG" if sub == "GET" => Some(("field", "value")),
        "HRANDFIELD" if with("WITHVALUES") => Some(("field", "value")),
        "ZPOPMIN" | "ZPOPMAX" => Some(("member", "score")),
        "ZRANGE" | "ZRANGEBYSCORE" | "ZREVRANGE" | "ZREVRANGEBYSCORE" | "ZRANDMEMBER"
        | "ZUNION" | "ZINTER" | "ZDIFF"
            if with("WITHSCORES") =>
        {
            Some(("member", "score"))
        }
        _ => None,
    }
}

/// A reply as a grid: arrays become rows, pairs two columns, the rest one cell.
pub fn result(reply: &Resp, parts: &[Vec<u8>]) -> QueryResult {
    let name = name_of(parts);
    let mut columns = vec!["value".to_string()];
    let rows: Vec<Row>;
    let mut counted = true;
    match reply {
        Resp::Array(items) => {
            if name == "XRANGE" || name == "XREVRANGE" {
                if let Some(entries) = stream_entries(items) {
                    columns = vec!["id".into(), "fields".into()];
                    return finish(columns, entries, &name, true);
                }
            }
            match pair_names(parts).zip(pairs(items)) {
                Some((names, pair_rows)) => {
                    columns = vec![names.0.into(), names.1.into()];
                    rows = pair_rows;
                }
                None => rows = items.iter().map(|i| vec![i.text()]).collect(),
            }
        }
        Resp::Map(flat) => {
            columns = vec!["field".into(), "value".into()];
            rows = flat
                .chunks(2)
                .filter(|c| c.len() == 2)
                .map(|c| vec![c[0].text(), c[1].text()])
                .collect();
        }
        other => {
            rows = vec![vec![other.text()]];
            counted = false;
        }
    }
    finish(columns, rows, &name, counted)
}

fn finish(columns: Vec<String>, rows: Vec<Row>, name: &str, counted: bool) -> QueryResult {
    let total = rows.len();
    let kept: Vec<Row> = rows.into_iter().take(ROW_LIMIT).collect();
    QueryResult {
        columns: columns.into_iter().map(|c| Column::new(c, "")).collect(),
        command: if counted {
            format!("{name} {total}")
        } else {
            name.to_string()
        },
        truncated: total > kept.len(),
        total_rows: total as u64,
        rows: kept,
        keys: None,
        note: None,
    }
}

/// A flat array of scalars of even length, or an array of 2-element arrays, as pairs.
fn pairs(items: &[Resp]) -> Option<Vec<Row>> {
    if items.iter().all(|i| !i.is_aggregate()) && items.len() % 2 == 0 {
        return Some(
            items
                .chunks(2)
                .map(|c| vec![c[0].text(), c[1].text()])
                .collect(),
        );
    }
    let mut rows = Vec::new();
    for item in items {
        match item {
            Resp::Array(pair)
                if pair.len() == 2 && !pair[0].is_aggregate() && !pair[1].is_aggregate() =>
            {
                rows.push(vec![pair[0].text(), pair[1].text()])
            }
            _ => return None,
        }
    }
    Some(rows)
}

/// XRANGE entries: [id, [field, value, ...]] as id and "field=value, ...".
fn stream_entries(items: &[Resp]) -> Option<Vec<Row>> {
    let mut rows = Vec::new();
    for item in items {
        let Resp::Array(entry) = item else {
            return None;
        };
        if entry.len() != 2 {
            return None;
        }
        let fields: &[Resp] = match &entry[1] {
            Resp::Array(flat) | Resp::Map(flat) => flat,
            Resp::Null => &[],
            _ => return None,
        };
        rows.push(vec![entry[0].text(), Some(field_text(fields))]);
    }
    Some(rows)
}

fn field_text(flat: &[Resp]) -> String {
    flat.chunks(2)
        .filter(|c| c.len() == 2)
        .map(|c| {
            format!(
                "{}={}",
                c[0].text().unwrap_or_default(),
                c[1].text().unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// "7.4.1" from INFO server; Valkey's own version when it gives one.
pub fn version(info: &str) -> String {
    let mut fields: HashMap<&str, &str> = HashMap::new();
    for line in info.lines() {
        if let Some((k, v)) = line.split_once(':') {
            fields.insert(k, v.trim());
        }
    }
    fields
        .get("valkey_version")
        .or_else(|| fields.get("redis_version"))
        .map(|v| v.to_string())
        .unwrap_or_default()
}

fn group_digits(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 {
        out.insert(0, '-');
    }
    out
}

// MARK: - Errors

fn gone() -> DbError {
    DbError::msg("This key no longer exists. Refresh the list of keys.")
}
fn row_changed() -> DbError {
    DbError::msg(
        "The row changed or was deleted since it was loaded. Reload the table and try again.",
    )
}
fn no_null() -> DbError {
    DbError::msg("Redis has no NULL. Enter a value instead.")
}
fn read_only_type(kind: &str) -> DbError {
    DbError::msg(format!(
        "{kind} keys can't be edited here. Use the command panel."
    ))
}
fn not_editable(column: &str) -> DbError {
    DbError::msg(format!("The {column} column can't be changed."))
}
fn exists(name: &str) -> DbError {
    DbError::msg(format!("“{name}” is already there. Pick another name."))
}
fn fill(column: &str) -> DbError {
    DbError::msg(format!("Enter a {column}."))
}

/// The new value for a column, if the edit changes it. NULL is refused.
fn change(changes: &[CellValue], column: &str) -> DbResult<Option<String>> {
    match changes.iter().rev().find(|c| c.column == column) {
        None => Ok(None),
        Some(c) => c.value.clone().map(Some).ok_or_else(no_null),
    }
}

fn outcome(sql: &str) -> EditOutcome {
    EditOutcome {
        sql: sql.to_string(),
        command: sql.split(' ').next().unwrap_or("").to_string(),
        affected: 1,
    }
}

fn nothing(command: &str) -> EditOutcome {
    EditOutcome {
        sql: String::new(),
        command: command.to_string(),
        affected: 0,
    }
}

// MARK: - Pages

/// A page before it becomes a QueryResult, with which rows hold bytes that aren't text.
struct Page {
    columns: Vec<Column>,
    rows: Vec<Row>,
    binary: Vec<bool>,
    keys: Option<RowKeys>,
    command: String,
    truncated: bool,
}

/// What a page showed, to check edits against.
struct ShownPage {
    keys: Vec<Row>,
    /// The last column of each row: the value a list index held.
    values: Vec<Cell>,
    binary: Vec<bool>,
}

fn key_rows(columns: &[&str], values: Vec<Row>) -> Option<RowKeys> {
    Some(RowKeys {
        columns: columns.iter().map(|c| c.to_string()).collect(),
        kind: RowKeyKind::PrimaryKey,
        values,
    })
}

/// The rows of a whole key that contain `search` (case-insensitive), sorted,
/// then one page of them.
fn filtered(
    page: Page,
    search: &str,
    sort: Option<&ColumnSort>,
    limit: usize,
    offset: usize,
) -> Page {
    let needle = search.to_lowercase();
    let mut hits: Vec<usize> = (0..page.rows.len())
        .filter(|&i| {
            needle.is_empty()
                || page.rows[i]
                    .iter()
                    .flatten()
                    .any(|c| c.to_lowercase().contains(&needle))
        })
        .collect();
    if let Some(sort) = sort {
        if let Some(column) = page.columns.iter().position(|c| c.name == sort.column) {
            hits.sort_by(|&a, &b| {
                ColumnSort::compare(
                    page.rows[a][column].as_deref(),
                    page.rows[b][column].as_deref(),
                    sort.ascending,
                )
            });
        }
    }
    let slice: Vec<usize> = hits.into_iter().skip(offset).take(limit).collect();
    let keys = match &page.keys {
        Some(k) if k.values.len() == page.rows.len() => Some(RowKeys {
            columns: k.columns.clone(),
            kind: k.kind,
            values: slice.iter().map(|&i| k.values[i].clone()).collect(),
        }),
        other => other.clone(),
    };
    Page {
        rows: slice.iter().map(|&i| page.rows[i].clone()).collect(),
        binary: slice
            .iter()
            .map(|&i| page.binary.get(i).copied().unwrap_or(false))
            .collect(),
        columns: page.columns,
        keys,
        command: page.command,
        truncated: page.truncated,
    }
}

fn scan_reply(reply: &Resp) -> DbResult<(String, Vec<Resp>)> {
    if let Resp::Array(parts) = reply {
        if let [cursor, Resp::Array(batch)] = parts.as_slice() {
            if let Some(cursor) = cursor.text() {
                return Ok((cursor, batch.clone()));
            }
        }
    }
    Err(DbError::msg("The server's SCAN reply wasn't understood."))
}

fn items(reply: Resp) -> Vec<Resp> {
    match reply {
        Resp::Array(items) => items,
        _ => vec![],
    }
}

// MARK: - Session

/// The connection and what the session remembers; one call at a time.
struct Inner {
    target: Target,
    link: Option<Link>,
    /// The database in use: the URL's, or the last SELECT run in the panel.
    database: u32,
    /// The rows of the last page of each key, to check edits against.
    shown: HashMap<String, ShownPage>,
}

/// One Redis or Valkey connection. Keys are the relations; each page shows what
/// a key holds, by type.
pub struct RedisSession {
    inner: Mutex<Inner>,
    /// `SELECT n` in the panel moved the connection off the URL's database.
    scope_changed: AtomicBool,
    version: String,
    running: AtomicBool,
    cancel_tx: watch::Sender<u64>,
}

/// Opens a session. Through an SSH tunnel, `local_port` replaces the URL's host and port.
pub async fn connect(url: &str, local_port: Option<u16>) -> DbResult<Box<dyn DatabaseSession>> {
    let url = RedisUrl::parse(url)?;
    let database = url.database;
    let target = Target::new(url, local_port);
    let link = target.open(database).await?;
    let mut inner = Inner {
        target,
        link: Some(link),
        database,
        shown: HashMap::new(),
    };
    let info = inner.command(&["INFO", "server"]).await?;
    let version = version(&info.text().unwrap_or_default());
    Ok(Box::new(RedisSession {
        inner: Mutex::new(inner),
        scope_changed: AtomicBool::new(false),
        version,
        running: AtomicBool::new(false),
        cancel_tx: watch::channel(0).0,
    }))
}

/// Runs one public call with the connection locked. `cancel()` drops the call
/// and the socket; the next call reconnects.
macro_rules! guarded {
    ($self:ident, $inner:ident => $body:expr) => {{
        let mut guard = $self.inner.lock().await;
        let mut cancelled = $self.cancel_tx.subscribe();
        $self.running.store(true, Ordering::SeqCst);
        let out = {
            let call = async {
                let $inner: &mut Inner = &mut *guard;
                $body
            };
            tokio::select! {
                r = call => Some(r),
                _ = cancelled.changed() => None,
            }
        };
        $self.running.store(false, Ordering::SeqCst);
        match out {
            Some(r) => r,
            None => {
                guard.link = None;
                Err(DbError::Cancelled)
            }
        }
    }};
}

impl Inner {
    async fn link(&mut self) -> DbResult<&mut Link> {
        if self.link.is_none() {
            self.link = Some(self.target.open(self.database).await?);
        }
        Ok(self.link.as_mut().expect("link was just opened"))
    }

    /// Sends commands in one write and reads every reply; error replies are returned, not thrown.
    async fn pipeline(&mut self, commands: Vec<Vec<Vec<u8>>>) -> DbResult<Vec<Resp>> {
        if commands.is_empty() {
            return Ok(vec![]);
        }
        let link = self.link().await?;
        let run = async {
            link.send(&commands).await?;
            let mut replies = Vec::with_capacity(commands.len());
            for _ in &commands {
                replies.push(link.read().await?);
            }
            Ok::<_, String>(replies)
        }
        .await;
        run.map_err(|message| {
            self.link = None;
            DbError::Message(message)
        })
    }

    /// One command; an error reply is returned as an error with the server's text.
    async fn command(&mut self, parts: &[&str]) -> DbResult<Resp> {
        self.raw(args(parts)).await
    }

    async fn raw(&mut self, parts: Vec<Vec<u8>>) -> DbResult<Resp> {
        match self.pipeline(vec![parts]).await?.remove(0) {
            Resp::Error(message) => Err(DbError::Message(message)),
            reply => Ok(reply),
        }
    }

    // The command panel

    async fn execute(&mut self, commands: Vec<Vec<Vec<u8>>>) -> DbResult<Vec<QueryResult>> {
        let mut results = Vec::new();
        for parts in commands {
            let reply = self.raw(parts.clone()).await?;
            match name_of(&parts).as_str() {
                "SELECT" => {
                    if reply == Resp::Simple("OK".into()) && parts.len() > 1 {
                        if let Ok(index) = String::from_utf8_lossy(&parts[1]).parse::<u32>() {
                            self.database = index;
                            self.shown.clear();
                        }
                    }
                }
                "QUIT" => self.link = None,
                _ => {}
            }
            results.push(result(&reply, &parts));
        }
        Ok(results)
    }

    // Browsing

    async fn relations(&mut self) -> DbResult<Vec<Relation>> {
        let mut cursor = "0".to_string();
        let mut seen = std::collections::HashSet::<Vec<u8>>::new();
        let mut keys: Vec<Vec<u8>> = Vec::new();
        loop {
            let reply = self.command(&["SCAN", &cursor, "COUNT", "1000"]).await?;
            let (next, batch) = scan_reply(&reply)?;
            for key in batch {
                if let Some(bytes) = key.bytes() {
                    if seen.insert(bytes.clone()) {
                        keys.push(bytes);
                    }
                }
            }
            cursor = next;
            if cursor == "0" || keys.len() >= KEY_LIMIT {
                break;
            }
        }
        keys.truncate(KEY_LIMIT);
        let mut relations = Vec::new();
        for chunk in keys.chunks(500) {
            let types = self
                .pipeline(
                    chunk
                        .iter()
                        .map(|k| vec![b"TYPE".to_vec(), k.clone()])
                        .collect(),
                )
                .await?;
            for (key, kind) in chunk.iter().zip(types) {
                if matches!(kind, Resp::Error(_)) {
                    continue;
                }
                let Some(subtype) = kind.text().filter(|t| t != "none") else {
                    continue;
                };
                let name = display(key);
                let schema = name
                    .find(':')
                    .map(|i| name[..i].to_string())
                    .unwrap_or_default();
                relations.push(Relation {
                    schema,
                    name,
                    kind: RelationKind::Key,
                    estimate: -1.0,
                    subtype,
                });
            }
        }
        relations.sort_by(|a, b| (&a.schema, &a.name).cmp(&(&b.schema, &b.name)));
        Ok(relations)
    }

    async fn page(
        &mut self,
        relation: &Relation,
        limit: usize,
        offset: usize,
        search: &str,
        sort: Option<&ColumnSort>,
    ) -> DbResult<QueryResult> {
        let key = relation.name.as_str();
        let head = self
            .pipeline(vec![args(&["TYPE", key]), args(&["TTL", key])])
            .await?;
        if let Resp::Error(message) = &head[0] {
            return Err(DbError::Message(message.clone()));
        }
        let kind = head[0].text().unwrap_or_else(|| "none".into());
        if kind == "none" {
            return Err(gone());
        }
        let ttl = head[1].integer().unwrap_or(-1);
        // Searching or sorting needs the whole key, then one page of it.
        let page = if search.is_empty() && sort.is_none() {
            self.read(key, &kind, limit, offset).await?
        } else {
            filtered(
                self.read(key, &kind, ROW_LIMIT, 0).await?,
                search,
                sort,
                limit,
                offset,
            )
        };
        self.shown.insert(
            relation.id(),
            ShownPage {
                keys: page
                    .keys
                    .as_ref()
                    .map(|k| k.values.clone())
                    .unwrap_or_default(),
                values: page
                    .rows
                    .iter()
                    .map(|r| r.last().cloned().flatten())
                    .collect(),
                binary: page.binary.clone(),
            },
        );
        Ok(QueryResult {
            columns: page.columns,
            total_rows: page.rows.len() as u64,
            rows: page.rows,
            command: page.command,
            truncated: page.truncated,
            keys: page.keys,
            note: Some(if ttl >= 0 {
                format!("{kind} · TTL {} s", group_digits(ttl))
            } else {
                format!("{kind} · no expiry")
            }),
        })
    }

    async fn read(&mut self, key: &str, kind: &str, limit: usize, offset: usize) -> DbResult<Page> {
        let col = |n: &str| Column::new(n, "");
        let single = |value: &Resp, command: &str, key: &str| Page {
            columns: vec![col("value")],
            rows: vec![vec![value.text()]],
            binary: vec![value.is_binary()],
            keys: key_rows(&["key"], vec![vec![Some(key.to_string())]]),
            command: command.into(),
            truncated: false,
        };
        match kind {
            "string" => {
                let value = self.command(&["GET", key]).await?;
                Ok(single(&value, "GET", key))
            }
            "hash" => {
                let (flat, truncated) = self.scan_all("HSCAN", key, true).await?;
                let mut by_field: HashMap<Vec<u8>, (Resp, Resp)> = HashMap::new();
                for pair in flat.chunks(2).filter(|p| p.len() == 2) {
                    by_field.insert(
                        pair[0].bytes().unwrap_or_default(),
                        (pair[0].clone(), pair[1].clone()),
                    );
                }
                let mut sorted: Vec<(Resp, Resp)> = by_field.into_values().collect();
                sorted.sort_by_key(|p| p.0.text().unwrap_or_default());
                let total = sorted.len();
                let slice: Vec<_> = sorted
                    .into_iter()
                    .take(ROW_LIMIT)
                    .skip(offset)
                    .take(limit)
                    .collect();
                Ok(Page {
                    columns: vec![col("field"), col("value")],
                    rows: slice.iter().map(|p| vec![p.0.text(), p.1.text()]).collect(),
                    binary: slice
                        .iter()
                        .map(|p| p.0.is_binary() || p.1.is_binary())
                        .collect(),
                    keys: key_rows(&["field"], slice.iter().map(|p| vec![p.0.text()]).collect()),
                    command: format!("HSCAN {}", slice.len()),
                    truncated: truncated || total > ROW_LIMIT,
                })
            }
            "list" => {
                let mut found = vec![];
                if limit > 0 {
                    let (from, to) = (offset.to_string(), (offset + limit - 1).to_string());
                    found = items(self.command(&["LRANGE", key, &from, &to]).await?);
                }
                Ok(Page {
                    columns: vec![Column::new("index", "integer"), col("value")],
                    rows: found
                        .iter()
                        .enumerate()
                        .map(|(i, v)| vec![Some((offset + i).to_string()), v.text()])
                        .collect(),
                    binary: found.iter().map(Resp::is_binary).collect(),
                    keys: key_rows(
                        &["index"],
                        (0..found.len())
                            .map(|i| vec![Some((offset + i).to_string())])
                            .collect(),
                    ),
                    command: format!("LRANGE {}", found.len()),
                    truncated: false,
                })
            }
            "set" => {
                let (flat, truncated) = self.scan_all("SSCAN", key, false).await?;
                let mut by_member: HashMap<Vec<u8>, Resp> = HashMap::new();
                for member in flat {
                    by_member.insert(member.bytes().unwrap_or_default(), member);
                }
                let mut sorted: Vec<Resp> = by_member.into_values().collect();
                sorted.sort_by_key(|m| m.text().unwrap_or_default());
                let total = sorted.len();
                let slice: Vec<_> = sorted
                    .into_iter()
                    .take(ROW_LIMIT)
                    .skip(offset)
                    .take(limit)
                    .collect();
                Ok(Page {
                    columns: vec![col("member")],
                    rows: slice.iter().map(|m| vec![m.text()]).collect(),
                    binary: slice.iter().map(Resp::is_binary).collect(),
                    keys: key_rows(&["member"], slice.iter().map(|m| vec![m.text()]).collect()),
                    command: format!("SSCAN {}", slice.len()),
                    truncated: truncated || total > ROW_LIMIT,
                })
            }
            "zset" => {
                let mut flat = vec![];
                if limit > 0 {
                    let (from, to) = (offset.to_string(), (offset + limit - 1).to_string());
                    flat = items(
                        self.command(&["ZRANGE", key, &from, &to, "WITHSCORES"])
                            .await?,
                    );
                }
                let pairs: Vec<(Resp, Resp)> = if flat.iter().all(|i| !i.is_aggregate()) {
                    flat.chunks(2)
                        .filter(|c| c.len() == 2)
                        .map(|c| (c[0].clone(), c[1].clone()))
                        .collect()
                } else {
                    flat.iter()
                        .filter_map(|i| match i {
                            Resp::Array(p) if p.len() == 2 => Some((p[0].clone(), p[1].clone())),
                            _ => None,
                        })
                        .collect()
                };
                Ok(Page {
                    columns: vec![col("member"), Column::new("score", "double")],
                    rows: pairs.iter().map(|p| vec![p.0.text(), p.1.text()]).collect(),
                    binary: pairs.iter().map(|p| p.0.is_binary()).collect(),
                    keys: key_rows(
                        &["member"],
                        pairs.iter().map(|p| vec![p.0.text()]).collect(),
                    ),
                    command: format!("ZRANGE {}", pairs.len()),
                    truncated: false,
                })
            }
            "stream" => {
                let mut entries = vec![];
                if limit > 0 {
                    let count = (offset + limit).to_string();
                    entries = items(
                        self.command(&["XRANGE", key, "-", "+", "COUNT", &count])
                            .await?,
                    );
                }
                let rows: Vec<Row> = stream_entries(&entries)
                    .unwrap_or_default()
                    .into_iter()
                    .skip(offset)
                    .collect();
                Ok(Page {
                    columns: vec![col("id"), col("fields")],
                    binary: vec![false; rows.len()],
                    command: format!("XRANGE {}", rows.len()),
                    rows,
                    keys: None,
                    truncated: false,
                })
            }
            "ReJSON-RL" => {
                let reply = self
                    .pipeline(vec![args(&[
                        "JSON.GET", key, "INDENT", "  ", "NEWLINE", "\n", "SPACE", " ",
                    ])])
                    .await?;
                Ok(single(&reply[0], "JSON.GET", key))
            }
            other => Ok(Page {
                columns: vec![col("value")],
                rows: vec![vec![Some(format!(
                    "dbLens can't show {other} keys yet. Use the command panel."
                ))]],
                binary: vec![false],
                keys: None,
                command: "TYPE".into(),
                truncated: false,
            }),
        }
    }

    /// Every element HSCAN or SSCAN returns, stopping a little past the row limit.
    async fn scan_all(
        &mut self,
        name: &str,
        key: &str,
        pairs: bool,
    ) -> DbResult<(Vec<Resp>, bool)> {
        let mut cursor = "0".to_string();
        let mut all: Vec<Resp> = Vec::new();
        let cap = ROW_LIMIT * if pairs { 2 } else { 1 };
        loop {
            let (next, batch) =
                scan_reply(&self.command(&[name, key, &cursor, "COUNT", "1000"]).await?)?;
            all.extend(batch);
            cursor = next;
            if cursor == "0" || all.len() > cap {
                break;
            }
        }
        Ok((all, cursor != "0"))
    }

    async fn count(&mut self, relation: &Relation, search: &str) -> DbResult<Option<u64>> {
        if !search.is_empty() {
            return Ok(Some(
                self.page(relation, ROW_LIMIT, 0, search, None)
                    .await?
                    .rows
                    .len() as u64,
            ));
        }
        let kind = self
            .command(&["TYPE", &relation.name])
            .await?
            .text()
            .unwrap_or_else(|| "none".into());
        let length = match kind.as_str() {
            "string" | "ReJSON-RL" => return Ok(Some(1)),
            "hash" => "HLEN",
            "list" => "LLEN",
            "set" => "SCARD",
            "zset" => "ZCARD",
            "stream" => "XLEN",
            _ => return Ok(None),
        };
        Ok(self
            .command(&[length, &relation.name])
            .await?
            .integer()
            .map(|n| n.max(0) as u64))
    }

    // Editing

    async fn current_type(&mut self, key: &str) -> DbResult<String> {
        let kind = self
            .command(&["TYPE", key])
            .await?
            .text()
            .unwrap_or_else(|| "none".into());
        if kind == "none" {
            return Err(row_changed());
        }
        Ok(kind)
    }

    /// The row as it was shown: its first key value and the value of its last
    /// column. Refuses rows whose values were shown as hex.
    fn shown_row(&self, relation: &Relation, key: &RowKey) -> DbResult<(Cell, Cell)> {
        let name = key.values.first().cloned().flatten();
        // No page on this connection: the grid's page was read on another one
        // (the pool moved the session), so the check can't run. Refuse.
        let Some(page) = self.shown.get(&relation.id()) else {
            return Err(row_changed());
        };
        let Some(row) = page.keys.iter().position(|k| k == &key.values) else {
            return Ok((name, None));
        };
        if page.binary.get(row).copied().unwrap_or(false) {
            return Err(DbError::msg(
                "This row holds bytes that aren't text (shown as hex), so it can't be edited here. Use the command panel.",
            ));
        }
        Ok((name, page.values.get(row).cloned().flatten()))
    }

    /// Starts an optimistic transaction: WATCH the key, then check, then `multi`.
    async fn watch(&mut self, key: &str) -> DbResult<()> {
        self.command(&["WATCH", key]).await.map(|_| ())
    }

    /// Ends the check of a WATCHed key: a failed or false check releases the
    /// watch and fails with the "row changed" message.
    async fn checked(&mut self, check: DbResult<bool>) -> DbResult<()> {
        match check {
            Ok(true) => Ok(()),
            Ok(false) => {
                self.command(&["UNWATCH"]).await?;
                Err(row_changed())
            }
            Err(e) => {
                let _ = self.command(&["UNWATCH"]).await;
                Err(e)
            }
        }
    }

    /// Runs the commands in MULTI/EXEC; a key changed meanwhile gives the "row changed" message.
    async fn multi(&mut self, commands: Vec<Vec<String>>) -> DbResult<Vec<Resp>> {
        let mut all = vec![args(&["MULTI"])];
        all.extend(
            commands
                .iter()
                .map(|c| c.iter().map(|p| p.as_bytes().to_vec()).collect()),
        );
        all.push(args(&["EXEC"]));
        let replies = self.pipeline(all).await?;
        let (last, queued) = replies.split_last().expect("MULTI and EXEC were sent");
        for reply in queued {
            if let Resp::Error(message) = reply {
                return Err(DbError::Message(message.clone()));
            }
        }
        match last {
            Resp::Error(message) => Err(DbError::Message(message.clone())),
            Resp::Array(results) => {
                for reply in results {
                    if let Resp::Error(message) = reply {
                        return Err(DbError::Message(message.clone()));
                    }
                }
                Ok(results.clone())
            }
            _ => Err(row_changed()),
        }
    }

    async fn update(
        &mut self,
        relation: &Relation,
        key: &RowKey,
        changes: &[CellValue],
    ) -> DbResult<EditOutcome> {
        if changes.is_empty() {
            return Ok(nothing("UPDATE 0"));
        }
        let name = relation.name.as_str();
        let kind = self.current_type(name).await?;
        let allowed: &[&str] = match kind.as_str() {
            "string" | "ReJSON-RL" | "list" => &["value"],
            "hash" => &["field", "value"],
            "set" => &["member"],
            "zset" => &["member", "score"],
            _ => &[],
        };
        if allowed.is_empty() {
            return Err(read_only_type(&kind));
        }
        if let Some(c) = changes
            .iter()
            .find(|c| !allowed.contains(&c.column.as_str()))
        {
            return Err(not_editable(&c.column));
        }
        let (row_key, row_value) = self.shown_row(relation, key)?;
        match kind.as_str() {
            "string" => {
                let Some(value) = change(changes, "value")? else {
                    return Ok(nothing("SET"));
                };
                if self
                    .command(&["SET", name, &value, "KEEPTTL", "XX"])
                    .await?
                    == Resp::Null
                {
                    return Err(row_changed());
                }
                Ok(outcome(&format!("SET {name} ? KEEPTTL XX")))
            }
            "ReJSON-RL" => {
                let Some(value) = change(changes, "value")? else {
                    return Ok(nothing("JSON.SET"));
                };
                if self.command(&["JSON.SET", name, "$", &value, "XX"]).await? == Resp::Null {
                    return Err(row_changed());
                }
                Ok(outcome(&format!("JSON.SET {name} $ ? XX")))
            }
            "hash" => self.update_hash(name, row_key, changes).await,
            "list" => {
                let Some(value) = change(changes, "value")? else {
                    return Ok(nothing("LSET"));
                };
                let (Some(index), Some(old)) = (row_key, row_value) else {
                    return Err(row_changed());
                };
                self.watch(name).await?;
                let check = self
                    .command(&["LINDEX", name, &index])
                    .await
                    .map(|r| r.bytes() == Some(old.into_bytes()));
                self.checked(check).await?;
                self.multi(vec![vec!["LSET".into(), name.into(), index.clone(), value]])
                    .await?;
                Ok(outcome(&format!("LSET {name} {index} ?")))
            }
            "set" => {
                let (Some(member), Some(old)) = (change(changes, "member")?, row_key) else {
                    return Ok(nothing("SADD"));
                };
                if member == old {
                    return Ok(outcome(&format!("SADD {name} ?")));
                }
                if self.command(&["SISMEMBER", name, &member]).await?.integer() == Some(1) {
                    return Err(exists(&member));
                }
                self.watch(name).await?;
                let check = self
                    .command(&["SISMEMBER", name, &old])
                    .await
                    .map(|r| r.integer() == Some(1));
                self.checked(check).await?;
                let results = self
                    .multi(vec![
                        vec!["SREM".into(), name.into(), old],
                        vec!["SADD".into(), name.into(), member],
                    ])
                    .await?;
                if !results.iter().all(|r| r.integer() == Some(1)) {
                    return Err(row_changed());
                }
                Ok(outcome(&format!("SREM {name} ?; SADD {name} ?")))
            }
            _ => self.update_sorted_set(name, row_key, changes).await,
        }
    }

    async fn update_hash(
        &mut self,
        key: &str,
        field: Cell,
        changes: &[CellValue],
    ) -> DbResult<EditOutcome> {
        let field = field.ok_or_else(row_changed)?;
        let new_field = change(changes, "field")?.unwrap_or_else(|| field.clone());
        let new_value = change(changes, "value")?;
        if new_field == field {
            let Some(new_value) = new_value else {
                return Ok(nothing("HSET"));
            };
            self.watch(key).await?;
            let check = self
                .command(&["HEXISTS", key, &field])
                .await
                .map(|r| r.integer() == Some(1));
            self.checked(check).await?;
            self.multi(vec![vec!["HSET".into(), key.into(), field, new_value]])
                .await?;
            return Ok(outcome(&format!("HSET {key} ? ?")));
        }
        // A new field name: HSET the new one and HDEL the old one together.
        if self.command(&["HEXISTS", key, &new_field]).await?.integer() == Some(1) {
            return Err(exists(&new_field));
        }
        self.watch(key).await?;
        let check = async {
            if self.command(&["HEXISTS", key, &field]).await?.integer() != Some(1) {
                return Ok::<_, DbError>(None);
            }
            match new_value.clone() {
                Some(v) => Ok(Some(v)),
                None => {
                    let old = self.command(&["HGET", key, &field]).await?;
                    Ok(if old.is_binary() { None } else { old.text() })
                }
            }
        }
        .await;
        let value = match check {
            Ok(Some(value)) => value,
            Ok(None) => {
                self.checked(Ok(false)).await?;
                return Err(row_changed());
            }
            Err(e) => {
                let _ = self.command(&["UNWATCH"]).await;
                return Err(e);
            }
        };
        let results = self
            .multi(vec![
                vec!["HSET".into(), key.into(), new_field, value],
                vec!["HDEL".into(), key.into(), field],
            ])
            .await?;
        if results.len() != 2 || results[1].integer() != Some(1) {
            return Err(row_changed());
        }
        Ok(outcome(&format!("HSET {key} ? ?; HDEL {key} ?")))
    }

    async fn update_sorted_set(
        &mut self,
        key: &str,
        member: Cell,
        changes: &[CellValue],
    ) -> DbResult<EditOutcome> {
        let member = member.ok_or_else(row_changed)?;
        let new_member = change(changes, "member")?.unwrap_or_else(|| member.clone());
        let new_score = change(changes, "score")?;
        if new_member == member {
            let Some(new_score) = new_score else {
                return Ok(nothing("ZADD"));
            };
            self.watch(key).await?;
            let check = self
                .command(&["ZSCORE", key, &member])
                .await
                .map(|r| r != Resp::Null);
            self.checked(check).await?;
            self.multi(vec![vec![
                "ZADD".into(),
                key.into(),
                "XX".into(),
                new_score,
                member,
            ]])
            .await?;
            return Ok(outcome(&format!("ZADD {key} XX ? ?")));
        }
        if self.command(&["ZSCORE", key, &new_member]).await? != Resp::Null {
            return Err(exists(&new_member));
        }
        self.watch(key).await?;
        let old = self
            .command(&["ZSCORE", key, &member])
            .await
            .map(|r| r.text());
        let score = match old {
            Ok(Some(old)) => new_score.unwrap_or(old),
            Ok(None) => {
                self.checked(Ok(false)).await?;
                return Err(row_changed());
            }
            Err(e) => {
                let _ = self.command(&["UNWATCH"]).await;
                return Err(e);
            }
        };
        let results = self
            .multi(vec![
                vec!["ZREM".into(), key.into(), member],
                vec!["ZADD".into(), key.into(), score, new_member],
            ])
            .await?;
        if !results.iter().all(|r| r.integer() == Some(1)) {
            return Err(row_changed());
        }
        Ok(outcome(&format!("ZREM {key} ?; ZADD {key} ? ?")))
    }

    /// Adds a field, item or member to a key. Columns left out are empty (score 0).
    async fn insert(&mut self, relation: &Relation, values: &[CellValue]) -> DbResult<EditOutcome> {
        let key = relation.name.as_str();
        let kind = self.current_type(key).await?;
        let value =
            |column: &str| -> DbResult<String> { Ok(change(values, column)?.unwrap_or_default()) };
        match kind.as_str() {
            "hash" => {
                let field = value("field")?;
                if field.is_empty() {
                    return Err(fill("field"));
                }
                if self
                    .command(&["HSETNX", key, &field, &value("value")?])
                    .await?
                    .integer()
                    != Some(1)
                {
                    return Err(exists(&field));
                }
                Ok(outcome(&format!("HSETNX {key} ? ?")))
            }
            "list" => {
                self.command(&["RPUSH", key, &value("value")?]).await?;
                Ok(outcome(&format!("RPUSH {key} ?")))
            }
            "set" => {
                let member = value("member")?;
                if self.command(&["SADD", key, &member]).await?.integer() != Some(1) {
                    return Err(exists(&member));
                }
                Ok(outcome(&format!("SADD {key} ?")))
            }
            "zset" => {
                let (member, score) = (value("member")?, value("score")?);
                let score = if score.is_empty() {
                    "0".to_string()
                } else {
                    score
                };
                if self
                    .command(&["ZADD", key, "NX", &score, &member])
                    .await?
                    .integer()
                    != Some(1)
                {
                    return Err(exists(&member));
                }
                Ok(outcome(&format!("ZADD {key} NX ? ?")))
            }
            "string" | "ReJSON-RL" => Err(DbError::msg(format!(
                "A {kind} key holds one value. Edit it instead of adding a row."
            ))),
            _ => Err(read_only_type(&kind)),
        }
    }

    /// Removes one field, item or member. Whole keys are deleted with DEL in the command panel.
    async fn delete(&mut self, relation: &Relation, key: &RowKey) -> DbResult<EditOutcome> {
        let name = relation.name.as_str();
        let kind = self.current_type(name).await?;
        // Kinds with no row deletes are refused first, so the message says what to do.
        match kind.as_str() {
            "hash" | "set" | "zset" | "list" => {}
            "string" | "ReJSON-RL" => {
                return Err(DbError::msg(
                    "To delete the whole key, run DEL in the command panel.",
                ))
            }
            _ => return Err(read_only_type(&kind)),
        }
        let (row_key, row_value) = self.shown_row(relation, key)?;
        let member = row_key.ok_or_else(row_changed)?;
        let removal = match kind.as_str() {
            "hash" => "HDEL",
            "set" => "SREM",
            "zset" => "ZREM",
            "list" => {
                let old = row_value.ok_or_else(row_changed)?;
                // LREM removes by value, so the item becomes a value nothing else holds first.
                let sentinel = format!("dblens:deleted:{}", uuid::Uuid::new_v4());
                self.watch(name).await?;
                let check = self
                    .command(&["LINDEX", name, &member])
                    .await
                    .map(|r| r.bytes() == Some(old.into_bytes()));
                self.checked(check).await?;
                let results = self
                    .multi(vec![
                        vec!["LSET".into(), name.into(), member.clone(), sentinel.clone()],
                        vec!["LREM".into(), name.into(), "1".into(), sentinel],
                    ])
                    .await?;
                if results.last().and_then(Resp::integer) != Some(1) {
                    return Err(row_changed());
                }
                return Ok(outcome(&format!("LSET {name} {member} ?; LREM {name} 1 ?")));
            }
            "string" | "ReJSON-RL" => {
                return Err(DbError::msg(
                    "To delete the whole key, run DEL in the command panel.",
                ))
            }
            _ => return Err(read_only_type(&kind)),
        };
        if self.command(&[removal, name, &member]).await?.integer() != Some(1) {
            return Err(row_changed());
        }
        Ok(outcome(&format!("{removal} {name} ?")))
    }
}

#[async_trait]
impl DatabaseSession for RedisSession {
    fn engine(&self) -> Engine {
        Engine::Redis
    }

    fn server_version(&self) -> String {
        self.version.clone()
    }

    /// Redis can't cancel a command on the server: this drops the running call
    /// and closes its socket (the next call reconnects). Never waits for the lock.
    fn cancel(&self) {
        if self.running.load(Ordering::SeqCst) {
            self.cancel_tx.send_modify(|n| *n += 1);
        }
    }

    /// Redis has no identifier quoting: the key as it is.
    fn quote(&self, identifier: &str) -> String {
        identifier.to_string()
    }

    /// A key is addressed by its full name.
    fn qualified(&self, relation: &Relation) -> String {
        relation.name.clone()
    }

    /// Runs commands, one per line, as redis-cli would. Blank lines and lines
    /// starting with # are skipped. An error stops the run with the server's text.
    async fn execute(&self, sql: &str) -> DbResult<Execution> {
        let start = Instant::now();
        let commands = commands_in(sql)?;
        for parts in &commands {
            let name = name_of(parts);
            if STREAMING.contains(&name.as_str()) {
                return Err(DbError::msg(format!(
                    "{name} never returns, so it can't run here. Use redis-cli for it."
                )));
            }
        }
        let results = guarded!(self, inner => {
            let results = inner.execute(commands).await;
            let moved = inner.database != inner.target.url.database;
            self.scope_changed.store(moved, Ordering::SeqCst);
            results
        })?;
        Ok(Execution::new(results, start.elapsed()))
    }

    async fn read_only(&self, text: &str) -> DbResult<Execution> {
        let commands = commands_in(text)?;
        if commands.len() != 1 {
            return Err(DbError::msg("Send one command at a time."));
        }
        let name = name_of(&commands[0]);
        if !READING_COMMANDS.contains(&name.as_str()) {
            return Err(DbError::msg(format!(
                "{name} can change data; read-only queries can't run it."
            )));
        }
        self.execute(text).await
    }

    async fn relations(&self) -> DbResult<Vec<Relation>> {
        guarded!(self, inner => inner.relations().await)
    }

    async fn page(
        &self,
        relation: &Relation,
        limit: u32,
        offset: u64,
        search: &str,
        sort: Option<&ColumnSort>,
    ) -> DbResult<QueryResult> {
        let (limit, offset) = (limit as usize, offset as usize);
        guarded!(self, inner => inner.page(relation, limit, offset, search, sort).await)
    }

    async fn count(&self, relation: &Relation, search: &str) -> DbResult<Option<u64>> {
        guarded!(self, inner => inner.count(relation, search).await)
    }

    async fn update(
        &self,
        relation: &Relation,
        key: &RowKey,
        changes: &[CellValue],
    ) -> DbResult<EditOutcome> {
        guarded!(self, inner => inner.update(relation, key, changes).await)
    }

    async fn insert(&self, relation: &Relation, values: &[CellValue]) -> DbResult<EditOutcome> {
        guarded!(self, inner => inner.insert(relation, values).await)
    }

    async fn delete(&self, relation: &Relation, key: &RowKey) -> DbResult<EditOutcome> {
        guarded!(self, inner => inner.delete(relation, key).await)
    }

    fn scope_changed(&self) -> bool {
        self.scope_changed.load(Ordering::SeqCst)
    }

    async fn close(&self) {
        self.inner.lock().await.link = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(parts: &[Vec<u8>]) -> Vec<String> {
        parts
            .iter()
            .map(|p| String::from_utf8_lossy(p).into_owned())
            .collect()
    }

    fn bulk(text: &str) -> Resp {
        Resp::Bulk(text.as_bytes().to_vec())
    }

    async fn parse(wire: &[u8]) -> Result<Resp, String> {
        let mut reader = BufReader::new(wire);
        read_resp(&mut reader).await
    }

    #[test]
    fn parses_urls() {
        let u = RedisUrl::parse("REDIS_URL=\"redis://ana:p%40ss@cache.example.com/3\"").unwrap();
        assert_eq!(
            (u.host.as_str(), u.port, u.database),
            ("cache.example.com", 6379, 3)
        );
        assert_eq!(
            (u.user.as_deref(), u.password.as_deref()),
            (Some("ana"), Some("p@ss"))
        );
        assert!(!u.tls && u.verify_tls);
        let u = RedisUrl::parse("redis://:secret@localhost:6380").unwrap();
        assert_eq!(
            (u.user, u.password.as_deref(), u.port),
            (None, Some("secret"), 6380)
        );
        let u = RedisUrl::parse("rediss://h?db=5&ssl_cert_reqs=none&password=x").unwrap();
        assert!(u.tls && !u.verify_tls);
        assert_eq!((u.database, u.password.as_deref()), (5, Some("x")));
        assert!(RedisUrl::parse("valkey://h").unwrap().host == "h");
        assert!(RedisUrl::parse("redis://h?tls=true").unwrap().tls);
        assert_eq!(RedisUrl::parse("redis://").unwrap().host, "localhost");
    }

    #[test]
    fn refuses_bad_urls() {
        assert!(RedisUrl::parse("").unwrap_err().message().contains("Paste"));
        assert!(RedisUrl::parse("redis+sentinel://h")
            .unwrap_err()
            .message()
            .contains("Sentinel"));
        assert!(RedisUrl::parse("redis-socket:///tmp/x")
            .unwrap_err()
            .message()
            .contains("Sentinel"));
        assert!(RedisUrl::parse("postgres://h/db")
            .unwrap_err()
            .message()
            .contains("Unrecognized"));
        assert!(RedisUrl::parse("redis://h/abc")
            .unwrap_err()
            .message()
            .contains("number"));
    }

    #[test]
    fn summary_names_the_database() {
        let s = summary("redis://cache.example.com:6380/2").unwrap();
        assert_eq!(s.suggested_name, "db 2 @ cache.example.com");
        assert_eq!(s.tls, None);
        assert_eq!(summary("rediss://h").unwrap().tls.as_deref(), Some("TLS"));
        assert!(summary("postgres://x/y").is_none());
    }

    #[test]
    fn tokenizes_like_redis_cli() {
        let t = |s: &str| words(&tokenize(s).unwrap());
        assert_eq!(t("SET  a   b"), ["SET", "a", "b"]);
        assert_eq!(t(r#"SET "a b" 'c d'"#), ["SET", "a b", "c d"]);
        assert_eq!(
            t(r#"SET k "line\nnext \"q\" \\ \x41""#),
            ["SET", "k", "line\nnext \"q\" \\ A"]
        );
        assert_eq!(t(r"SET k 'it\'s \n'"), ["SET", "k", "it's \\n"]);
        assert_eq!(t(r#"SET k """#), ["SET", "k", ""]);
        assert_eq!(t("SET a\"b c"), ["SET", "a\"b", "c"]);
        assert_eq!(tokenize(r"SET k '\xff'").unwrap()[2], b"\\xff".to_vec());
        assert_eq!(tokenize(r#"SET k "\xff""#).unwrap()[2], vec![0xff]);
        assert!(tokenize(r#"SET "a"b"#).is_err());
        assert!(tokenize("SET 'open")
            .unwrap_err()
            .message()
            .contains("Unbalanced quotes"));
    }

    #[test]
    fn splits_command_lines() {
        let c = commands_in("# comment\n\nPING\n  SET a 1  \r\n\t\nGET a").unwrap();
        assert_eq!(c.len(), 3);
        assert_eq!(name_of(&c[1]), "SET");
        assert_eq!(name_of(&[b"get".to_vec()]), "GET");
    }

    #[test]
    fn encodes_resp() {
        assert_eq!(
            encode(&[args(&["GET", "ab"])]),
            b"*2\r\n$3\r\nGET\r\n$2\r\nab\r\n"
        );
    }

    #[tokio::test]
    async fn reads_resp_replies() {
        assert_eq!(parse(b"+OK\r\n").await, Ok(Resp::Simple("OK".into())));
        assert_eq!(
            parse(b"-ERR nope\r\n").await,
            Ok(Resp::Error("ERR nope".into()))
        );
        assert_eq!(parse(b":-42\r\n").await, Ok(Resp::Integer(-42)));
        assert_eq!(parse(b"$5\r\nhello\r\n").await, Ok(bulk("hello")));
        assert_eq!(parse(b"$-1\r\n").await, Ok(Resp::Null));
        assert_eq!(parse(b"_\r\n").await, Ok(Resp::Null));
        assert_eq!(parse(b"$3\r\na\r\n\r\n").await, Ok(bulk("a\r\n")));
        assert_eq!(
            parse(b"*2\r\n$1\r\na\r\n*1\r\n:1\r\n").await,
            Ok(Resp::Array(vec![
                bulk("a"),
                Resp::Array(vec![Resp::Integer(1)])
            ]))
        );
        assert_eq!(
            parse(b"%1\r\n+k\r\n:2\r\n").await,
            Ok(Resp::Map(vec![Resp::Simple("k".into()), Resp::Integer(2)]))
        );
        assert_eq!(parse(b"#t\r\n").await, Ok(Resp::Boolean(true)));
        assert_eq!(parse(b",3.5\r\n").await, Ok(Resp::Double("3.5".into())));
        assert_eq!(parse(b"=7\r\ntxt:abc\r\n").await, Ok(bulk("abc")));
        assert_eq!(
            parse(b"|1\r\n+a\r\n+b\r\n:7\r\n").await,
            Ok(Resp::Integer(7))
        );
        assert!(parse(b"?x\r\n")
            .await
            .unwrap_err()
            .contains("unexpected reply type"));
        assert!(parse(b"$3\r\nabcXX").await.unwrap_err().contains("CRLF"));
        assert!(parse(b"$3\r\nab").await.unwrap_err().contains("closed"));
        assert!(parse(b"").await.unwrap_err().contains("closed"));
    }

    #[test]
    fn shows_cells() {
        assert_eq!(bulk("hi").text().as_deref(), Some("hi"));
        assert_eq!(
            Resp::Bulk(vec![0xff, 0x00, 0xab]).text().as_deref(),
            Some("0xFF00AB")
        );
        let long = display(&[0xffu8; 70]);
        assert!(
            long.starts_with("0xFFFF")
                && long.ends_with('…')
                && long.len() == 2 + 128 + '…'.len_utf8()
        );
        assert!(Resp::Bulk(vec![0xff]).is_binary() && !bulk("x").is_binary());
        assert_eq!(Resp::Null.text(), None);
        let nested = Resp::Array(vec![
            bulk("a\"b\n"),
            Resp::Integer(1),
            Resp::Null,
            Resp::Map(vec![bulk("k"), bulk("v")]),
        ]);
        assert_eq!(nested.text().unwrap(), r#"["a\"b\n", 1, null, {"k": "v"}]"#);
        assert_eq!(bulk("12").integer(), Some(12));
        assert_eq!(group_digits(3600), "3,600");
        assert_eq!(group_digits(12), "12");
    }

    #[test]
    fn shapes_replies_as_grids() {
        let parts = |s: &str| tokenize(s).unwrap();
        let hash = Resp::Array(vec![bulk("f1"), bulk("v1"), bulk("f2"), bulk("v2")]);
        let r = result(&hash, &parts("HGETALL h"));
        assert_eq!(
            r.columns
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            ["field", "value"]
        );
        assert_eq!((r.rows.len(), r.command.as_str()), (2, "HGETALL 2"));
        let plain = result(
            &Resp::Array(vec![bulk("a"), bulk("b")]),
            &parts("LRANGE l 0 -1"),
        );
        assert_eq!(plain.columns[0].name, "value");
        assert_eq!(
            plain.rows,
            vec![vec![Some("a".into())], vec![Some("b".into())]]
        );
        let scores = result(
            &Resp::Array(vec![bulk("m"), bulk("1.5")]),
            &parts("zrange z 0 -1 withscores"),
        );
        assert_eq!(scores.columns[1].name, "score");
        let nested = result(
            &Resp::Array(vec![Resp::Array(vec![bulk("m"), Resp::Double("2".into())])]),
            &parts("ZRANGE z 0 -1 WITHSCORES"),
        );
        assert_eq!(nested.rows[0], vec![Some("m".into()), Some("2".into())]);
        let stream = Resp::Array(vec![Resp::Array(vec![
            bulk("1-0"),
            Resp::Array(vec![bulk("a"), bulk("1"), bulk("b"), bulk("2")]),
        ])]);
        let s = result(&stream, &parts("XRANGE s - +"));
        assert_eq!(s.rows[0], vec![Some("1-0".into()), Some("a=1, b=2".into())]);
        let single = result(&Resp::Simple("OK".into()), &parts("SET a 1"));
        assert_eq!(
            (single.command.as_str(), single.rows[0][0].as_deref()),
            ("SET", Some("OK"))
        );
        let map = result(
            &Resp::Map(vec![bulk("k"), Resp::Integer(1)]),
            &parts("HELLO 3"),
        );
        assert_eq!(map.columns[0].name, "field");
        let big = result(
            &Resp::Array(
                (0..ROW_LIMIT + 5)
                    .map(|i| Resp::Integer(i as i64))
                    .collect(),
            ),
            &parts("SMEMBERS s"),
        );
        assert!(
            big.truncated && big.rows.len() == ROW_LIMIT && big.total_rows == ROW_LIMIT as u64 + 5
        );
    }

    #[test]
    fn reads_the_version() {
        assert_eq!(
            version("# Server\r\nredis_version:7.4.1\r\nos:Linux\r\n"),
            "7.4.1"
        );
        assert_eq!(
            version("redis_version:7.2.4\r\nvalkey_version:8.0.1\r\n"),
            "8.0.1"
        );
        assert_eq!(version("nothing"), "");
    }

    #[test]
    fn filters_and_sorts_a_whole_key() {
        let page = Page {
            columns: vec![Column::new("field", ""), Column::new("value", "")],
            rows: vec![
                vec![Some("a".into()), Some("10".into())],
                vec![Some("b".into()), Some("9".into())],
                vec![Some("c".into()), Some("Hello".into())],
            ],
            binary: vec![false, true, false],
            keys: key_rows(
                &["field"],
                vec![
                    vec![Some("a".into())],
                    vec![Some("b".into())],
                    vec![Some("c".into())],
                ],
            ),
            command: "HSCAN 3".into(),
            truncated: false,
        };
        let sort = ColumnSort {
            column: "value".into(),
            ascending: true,
        };
        let by_value = filtered(
            Page {
                rows: page.rows.clone(),
                keys: page.keys.clone(),
                columns: page.columns.clone(),
                binary: page.binary.clone(),
                command: page.command.clone(),
                ..page
            },
            "",
            Some(&sort),
            2,
            0,
        );
        assert_eq!(by_value.rows[0][0].as_deref(), Some("b"));
        assert_eq!(
            by_value.keys.as_ref().unwrap().values[1],
            vec![Some("a".into())]
        );
        assert_eq!(by_value.binary, vec![true, false]);
        let page2 = Page {
            columns: by_value.columns.clone(),
            rows: vec![
                vec![Some("c".into()), Some("Hello".into())],
                vec![Some("d".into()), None],
            ],
            binary: vec![false; 2],
            keys: None,
            command: String::new(),
            truncated: false,
        };
        let hit = filtered(page2, "HELL", None, 10, 0);
        assert_eq!(hit.rows.len(), 1);
    }

    #[test]
    fn edit_helpers() {
        let c = |col: &str, v: Option<&str>| CellValue {
            column: col.into(),
            value: v.map(String::from),
        };
        assert_eq!(
            change(&[c("value", Some("a")), c("value", Some("b"))], "value")
                .unwrap()
                .as_deref(),
            Some("b")
        );
        assert_eq!(change(&[c("x", Some("a"))], "value").unwrap(), None);
        assert!(change(&[c("value", None)], "value")
            .unwrap_err()
            .message()
            .contains("no NULL"));
        let o = outcome("HSET k ? ?");
        assert_eq!((o.command.as_str(), o.affected), ("HSET", 1));
    }

    #[test]
    fn read_only_list_has_no_writers() {
        for writer in [
            "SET", "DEL", "FLUSHALL", "HSET", "EVAL", "CONFIG", "SELECT", "EXPIRE",
        ] {
            assert!(!READING_COMMANDS.contains(&writer), "{writer}");
        }
        assert!(READING_COMMANDS.contains(&"GET") && READING_COMMANDS.contains(&"JSON.GET"));
    }

    #[test]
    fn tls_config_builds() {
        assert!(tls::config(true).is_ok());
        assert!(tls::config(false).is_ok());
    }
}
