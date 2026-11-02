//! Connection text as people paste it. Port of `ConnectionURL.swift` (Postgres
//! parsing, cleaning, masking passwords), `URLParts` (any scheme://… URL) and
//! `SQLiteLocation` (a file path or sqlite:/file: URL).

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use percent_encoding::{percent_decode_str, utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use regex::{Captures, Regex};
use serde::Serialize;

use crate::session::DbError;

/// What stands in for a saved password on screen.
pub const MASK: &str = "•••";

/// Settings whose values are secret: libpq's, and MongoDB's key file and proxy
/// passwords (also the old `sslPEMKeyPassword` spelling).
const SECRET_KEYS: [&str; 5] = [
    "password",
    "sslpassword",
    "tlsCertificateKeyFilePassword",
    "sslPEMKeyPassword",
    "proxyPassword",
];

/// The AWS session token, one entry of MongoDB's `authMechanismProperties`
/// list (`AWS_SESSION_TOKEN:…`, the colon maybe as %3A). Group 2 is the token.
fn aws_token_pattern(value: &str) -> Regex {
    Regex::new(&format!(
        r"(?i)([?&]authMechanismProperties=(?:[^&#]*?(?:,|%2C))?AWS_SESSION_TOKEN(?::|%3A))({value})()"
    ))
    .expect("token pattern")
}

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("valid regex"))
}

// MARK: - Cleaning

/// Strips what comes around a pasted URL: spaces, an .env name (`DATABASE_URL=`,
/// `export X=`), quotes. A `key=value` libpq string is left alone.
pub fn clean(text: &str) -> String {
    static ENV: OnceLock<Regex> = OnceLock::new();
    static SCHEME: OnceLock<Regex> = OnceLock::new();
    let mut value = text.trim().to_string();
    if let Some(rest) = value.strip_prefix("export ") {
        value = rest.to_string();
    }
    // DATABASE_URL=postgres://… (only when what follows is a URL, so host=… stays)
    if let Some(m) = re(&ENV, r"^[A-Za-z_][A-Za-z0-9_]*\s*=\s*").find(&value) {
        let after = value[m.end()..].to_string();
        let rest = after.trim_matches(|c| c == '"' || c == '\'' || c == ' ');
        if re(&SCHEME, r"^(?i)(jdbc:)?[a-z][a-z0-9+.-]*:").is_match(rest) {
            value = after;
        }
    }
    let mut value = value
        .trim_matches(|c: char| c == ' ' || c == '\t')
        .to_string();
    let chars: Vec<char> = value.chars().collect();
    if chars.len() >= 2 {
        let (first, last) = (chars[0], chars[chars.len() - 1]);
        if first == last && (first == '"' || first == '\'') {
            value = chars[1..chars.len() - 1].iter().collect();
        }
    }
    value.trim().to_string()
}

fn decode(text: &str) -> String {
    percent_decode_str(text)
        .decode_utf8()
        .map(|s| s.into_owned())
        .unwrap_or_else(|_| text.to_string())
}

/// Everything but RFC 3986's unreserved characters is percent-encoded.
const UNRESERVED: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

pub fn encode(text: &str) -> String {
    utf8_percent_encode(text, UNRESERVED).to_string()
}

/// Quotes a name for SQL the Postgres/SQLite way: my"table -> "my""table"
pub fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// `C:\…`, `C:/…` or `\\server\share`, the absolute paths of Windows.
pub fn is_windows_absolute(text: &str) -> bool {
    let b = text.as_bytes();
    (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/'))
        || text.starts_with("\\\\")
}

// MARK: - Postgres

/// Why a text isn't a Postgres connection.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UrlProblem {
    #[error("Paste the database URL.")]
    Empty,
    #[error("Unrecognized format. Use a postgres://… URL (or postgresql://, jdbc:postgresql://) or the host=… dbname=… user=… format")]
    NotPostgres,
}

impl From<UrlProblem> for DbError {
    fn from(problem: UrlProblem) -> Self {
        DbError::Message(problem.to_string())
    }
}

/// A Postgres connection, as pasted: any of the usual ways people write it.
///   postgres://user:pass@host:5432/db?sslmode=require   (also postgresql://)
///   postgresql://user@host1:5432,host2:5433/db           (several hosts)
///   postgresql:///db?host=/var/run/postgresql            (local socket)
///   jdbc:postgresql://host/db?user=ana&password=…&ssl=true
///   postgresql+psycopg2://…  postgres+asyncpg://…          (SQLAlchemy)
///   DATABASE_URL="postgres://…?schema=public"            (an .env line, Prisma)
///   host=db.example.com port=5432 dbname=loja user=ana   (libpq key=value)
/// Everything becomes libpq's own key=value settings. Driver-only options are
/// translated (ssl=true, sslmode=no-verify, schema=…) or dropped.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ConnectionUrl {
    /// What was pasted, without an .env name or quotes around it.
    pub raw: String,
    /// libpq settings: host, port, dbname, user, password, sslmode, …
    pub settings: BTreeMap<String, String>,
}

/// The settings libpq knows (PostgreSQL 16–18). Anything else is dropped.
const LIBPQ_KEYS: [&str; 46] = [
    "host",
    "hostaddr",
    "port",
    "dbname",
    "user",
    "password",
    "passfile",
    "channel_binding",
    "connect_timeout",
    "client_encoding",
    "options",
    "application_name",
    "fallback_application_name",
    "keepalives",
    "keepalives_idle",
    "keepalives_interval",
    "keepalives_count",
    "tcp_user_timeout",
    "replication",
    "gssencmode",
    "sslmode",
    "requiressl",
    "sslcompression",
    "sslcert",
    "sslkey",
    "sslpassword",
    "sslcertmode",
    "sslrootcert",
    "sslcrl",
    "sslcrldir",
    "sslsni",
    "requirepeer",
    "ssl_min_protocol_version",
    "ssl_max_protocol_version",
    "krbsrvname",
    "gsslib",
    "gssdelegation",
    "service",
    "target_session_attrs",
    "load_balance_hosts",
    "sslnegotiation",
    "require_auth",
    "sslkeylogfile",
    "scram_client_key",
    "scram_server_key",
    "oauth_issuer",
];

const PREFERRED: [&str; 7] = [
    "host", "hostaddr", "port", "dbname", "user", "password", "sslmode",
];

impl ConnectionUrl {
    pub fn new(text: &str) -> Result<Self, UrlProblem> {
        let cleaned = clean(text);
        if cleaned.is_empty() {
            return Err(UrlProblem::Empty);
        }
        let parsed = if let Some(rest) = url_form(&cleaned) {
            Some(parse_url(rest))
        } else if cleaned.contains('=') {
            parse_key_value(&cleaned)
        } else {
            None
        };
        let parsed = parsed
            .filter(|p| !p.is_empty())
            .ok_or(UrlProblem::NotPostgres)?;
        let settings = normalize(&parsed);
        // key=value text with no setting libpq knows is not a connection.
        if settings.is_empty() {
            return Err(UrlProblem::NotPostgres);
        }
        Ok(Self {
            raw: cleaned,
            settings,
        })
    }

    /// All hosts, comma separated, as libpq takes them. Empty means the local socket.
    pub fn hosts(&self) -> &str {
        self.settings.get("host").map(String::as_str).unwrap_or("")
    }

    /// The first host, for names and the form.
    pub fn host(&self) -> String {
        let first = self.hosts().split(',').next().unwrap_or("");
        if first.is_empty() || first.starts_with('/') {
            "localhost".into()
        } else {
            first.into()
        }
    }

    pub fn port(&self) -> Option<u16> {
        self.settings.get("port")?.split(',').next()?.parse().ok()
    }

    pub fn user(&self) -> Option<&str> {
        self.settings.get("user").map(String::as_str)
    }

    pub fn password(&self) -> Option<&str> {
        self.settings.get("password").map(String::as_str)
    }

    pub fn sslmode(&self) -> Option<&str> {
        self.settings.get("sslmode").map(String::as_str)
    }

    /// libpq's default database is the user's name.
    pub fn database(&self) -> String {
        self.settings
            .get("dbname")
            .cloned()
            .or_else(|| self.user().map(String::from))
            .unwrap_or_else(|| "postgres".into())
    }

    /// "loja @ db.example.com" for the sidebar.
    pub fn suggested_name(&self) -> String {
        format!("{} @ {}", self.database(), self.host())
    }

    /// What was pasted, with passwords hidden, safe to show on screen.
    pub fn redacted(&self) -> String {
        redact(&self.raw)
    }

    /// libpq key=value settings, plus a connect timeout so a wrong host fails
    /// in seconds instead of hanging. tokio-postgres reads this form too, but
    /// knows fewer keys: the Postgres driver picks what it needs from `settings`.
    pub fn for_libpq(&self) -> String {
        libpq(self.settings.clone())
    }

    /// The same settings aimed at a local port, for a connection through an SSH tunnel.
    pub fn for_libpq_through_local_port(&self, port: u16) -> String {
        let mut all = self.settings.clone();
        all.insert("host".into(), "127.0.0.1".into());
        all.insert("port".into(), port.to_string());
        all.remove("hostaddr");
        libpq(all)
    }

    /// Settings for the psql command line: no password (it goes in PGPASSWORD) and,
    /// through a tunnel, the local port.
    pub fn conninfo_without_password(&self, local_port: Option<u16>) -> String {
        let mut all = self.settings.clone();
        all.remove("password");
        all.remove("sslpassword");
        if let Some(port) = local_port {
            all.insert("host".into(), "127.0.0.1".into());
            all.insert("port".into(), port.to_string());
            all.remove("hostaddr");
        }
        ordered(&all)
    }
}

fn libpq(mut values: BTreeMap<String, String>) -> String {
    values
        .entry("connect_timeout".into())
        .or_insert_with(|| "10".into());
    ordered(&values)
}

fn ordered(values: &BTreeMap<String, String>) -> String {
    let rank = |key: &str| {
        PREFERRED
            .iter()
            .position(|p| *p == key)
            .unwrap_or(PREFERRED.len())
    };
    let mut pairs: Vec<(&String, &String)> = values.iter().collect();
    pairs.sort_by(|a, b| rank(a.0).cmp(&rank(b.0)).then_with(|| a.0.cmp(b.0)));
    pairs
        .iter()
        .map(|(k, v)| format!("{k}='{}'", v.replace('\\', "\\\\").replace('\'', "\\'")))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The part after the scheme, for postgres://, postgresql://, jdbc:… and postgresql+driver://.
fn url_form(text: &str) -> Option<&str> {
    static FORM: OnceLock<Regex> = OnceLock::new();
    re(&FORM, r"^(?i)(jdbc:)?postgres(ql)?(\+[a-z0-9_]+)?://")
        .find(text)
        .map(|m| &text[m.end()..])
}

/// scheme-less `user:pass@host1:1,host2:2/db?k=v#frag` into libpq-style keys
/// (user, password, host, port, dbname, and every query parameter as written).
pub(crate) fn parse_url(rest: &str) -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();
    let mut body = rest.split('#').next().unwrap_or("").to_string();
    let mut query = String::new();
    if let Some(mark) = body.find('?') {
        query = body[mark + 1..].to_string();
        body.truncate(mark);
    }
    let mut authority = body.clone();
    if let Some(slash) = body.find('/') {
        authority = body[..slash].to_string();
        let path = &body[slash + 1..];
        if !path.is_empty() {
            values.insert("dbname".into(), decode(path));
        }
    }
    if let Some(at) = authority.rfind('@') {
        let userinfo = authority[..at].to_string();
        authority = authority[at + 1..].to_string();
        let mut parts = userinfo.splitn(2, ':');
        if let Some(user) = parts.next().filter(|u| !u.is_empty()) {
            values.insert("user".into(), decode(user));
        }
        if let Some(password) = parts.next() {
            values.insert("password".into(), decode(password));
        }
    }
    let mut hosts: Vec<String> = Vec::new();
    let mut ports: Vec<String> = Vec::new();
    for entry in authority.split(',').filter(|e| !e.is_empty()) {
        if let (true, Some(close)) = (entry.starts_with('['), entry.find(']')) {
            hosts.push(entry[1..close].to_string());
            let after = &entry[close + 1..];
            ports.push(after.strip_prefix(':').unwrap_or("").to_string());
        } else if let Some(colon) = entry
            .rfind(':')
            .filter(|c| entry[c + 1..].chars().all(char::is_numeric))
        {
            hosts.push(decode(&entry[..colon]));
            ports.push(entry[colon + 1..].to_string());
        } else {
            hosts.push(decode(entry));
            ports.push(String::new());
        }
    }
    if hosts.iter().any(|h| !h.is_empty()) {
        values.insert("host".into(), hosts.join(","));
    }
    if ports.iter().any(|p| !p.is_empty()) {
        values.insert("port".into(), ports.join(","));
    }
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let mut kv = pair.splitn(2, '=');
        let key = kv.next().unwrap_or("");
        if key.is_empty() {
            continue;
        }
        values.insert(decode(key), kv.next().map(decode).unwrap_or_default());
    }
    values
}

/// libpq's key=value form: host=x port=5432 password='it\'s' (values may be quoted).
fn parse_key_value(text: &str) -> Option<BTreeMap<String, String>> {
    let chars: Vec<char> = text.chars().collect();
    let mut values = BTreeMap::new();
    let mut i = 0;
    let skip = |i: &mut usize| {
        while *i < chars.len() && chars[*i].is_whitespace() {
            *i += 1;
        }
    };
    loop {
        skip(&mut i);
        if i >= chars.len() {
            break;
        }
        let start = i;
        while i < chars.len() && chars[i] != '=' && !chars[i].is_whitespace() {
            i += 1;
        }
        let key: String = chars[start..i].iter().collect();
        skip(&mut i);
        if key.is_empty() || i >= chars.len() || chars[i] != '=' {
            return None;
        }
        i += 1;
        skip(&mut i);
        let mut value = String::new();
        if i < chars.len() && chars[i] == '\'' {
            i += 1;
            let mut closed = false;
            while i < chars.len() {
                let c = chars[i];
                if c == '\\' && i + 1 < chars.len() {
                    i += 1;
                    value.push(chars[i]);
                } else if c == '\'' {
                    closed = true;
                    i += 1;
                    break;
                } else {
                    value.push(c);
                }
                i += 1;
            }
            if !closed {
                return None;
            }
        } else {
            while i < chars.len() && !chars[i].is_whitespace() {
                value.push(chars[i]);
                i += 1;
            }
        }
        values.insert(key, value);
    }
    (!values.is_empty()).then_some(values)
}

fn normalize(input: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut values: BTreeMap<String, String> = BTreeMap::new();
    let mut schema: Option<String> = None;
    let has_sslmode = input.keys().any(|k| k == "sslmode");
    // Explicit keys win over their aliases whatever the order.
    let mut aliases: Vec<(&str, String)> = Vec::new();
    for (raw_key, value) in input {
        let key = raw_key.to_lowercase();
        match key.as_str() {
            "username" => aliases.push(("user", value.clone())),
            "database" | "db" => aliases.push(("dbname", value.clone())),
            "applicationname" => {
                values.insert("application_name".into(), value.clone());
            }
            "schema" | "currentschema" | "search_path" => schema = Some(value.clone()),
            "ssl" => {
                if ["true", "1", "on", "require"].contains(&value.to_lowercase().as_str())
                    && !has_sslmode
                {
                    values.insert("sslmode".into(), "require".into());
                }
            }
            "sslmode" => {
                // node-postgres' no-verify: encrypted, certificate not checked = libpq's require.
                let mode = value.to_lowercase();
                values.insert(
                    "sslmode".into(),
                    if mode == "no-verify" {
                        "require".into()
                    } else {
                        mode
                    },
                );
            }
            _ => {
                if LIBPQ_KEYS.contains(&key.as_str()) {
                    values.insert(key, value.clone());
                }
            }
        }
    }
    for (key, value) in aliases {
        values.entry(key.into()).or_insert(value);
    }
    if let Some(schema) = schema.filter(|s| !s.is_empty()) {
        let option = format!("-c search_path={schema}");
        let combined = match values.get("options") {
            Some(existing) => format!("{existing} {option}"),
            None => option,
        };
        values.insert("options".into(), combined);
    }
    values
}

// MARK: - Masking

#[derive(Clone, Copy, PartialEq, Eq)]
enum Place {
    Userinfo,
    Query,
    KeyValue,
}

impl Place {
    fn all_for(key: &str) -> &'static [Place] {
        if key == "password" {
            &[Place::Userinfo, Place::Query, Place::KeyValue]
        } else {
            &[Place::Query, Place::KeyValue]
        }
    }

    /// What a value looks like there.
    fn value(self) -> &'static str {
        match self {
            Place::Userinfo => r"[^/?#]*",
            Place::Query => r"[^&#]*",
            Place::KeyValue => r"(?:'(?:[^'\\]|\\.)*'|\S+)",
        }
    }

    /// Group 1 is the text before the value, group 2 the value, group 3
    /// (userinfo only) what follows it. Rust's regex has no lookahead, so the
    /// host after the password is matched and put back.
    fn pattern(self, key: &str, value: &str) -> Regex {
        let pattern = match self {
            // user:password@host; the password may hold "@", so up to the last one.
            Place::Userinfo => format!(r"(://[^:/?#@]*:)({value})(@[^@/?#]*(?:[/?#]|$))"),
            Place::Query => format!(r"(?i)([?&]{key}=)({value})()"),
            Place::KeyValue => format!(r"(?i)((?:^|\s){key}\s*=\s*)({value})()"),
        };
        Regex::new(&pattern).expect("mask pattern")
    }
}

/// Any connection text (any engine) with its passwords hidden.
pub fn redact(raw: &str) -> String {
    let mut text = raw.to_string();
    for key in SECRET_KEYS {
        for place in Place::all_for(key) {
            let pattern = place.pattern(key, place.value());
            text = pattern
                .replace_all(&text, |c: &Captures| format!("{}{MASK}{}", &c[1], &c[3]))
                .into_owned();
        }
    }
    aws_token_pattern(r"[^,&#]*")
        .replace_all(&text, |c: &Captures| format!("{}{MASK}", &c[1]))
        .into_owned()
}

/// The password and sslpassword in any connection text.
pub fn secrets(text: &str) -> BTreeMap<String, String> {
    // Postgres: libpq's own settings (keys lowercased, key=value form too).
    if let Ok(url) = ConnectionUrl::new(text) {
        return url.settings;
    }
    let mut found = BTreeMap::new();
    if let Some(parts) = UrlParts::new(text) {
        let query = |key: &str| {
            parts
                .query
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(key))
                .map(|(_, v)| v.clone())
        };
        if let Some(password) = parts.password.clone().or_else(|| query("password")) {
            found.insert("password".into(), password);
        }
        for key in &SECRET_KEYS[1..] {
            if let Some(value) = query(key) {
                found.insert((*key).into(), value);
            }
        }
        if let Some(token) = query("authMechanismProperties").and_then(|list| {
            list.split(',').find_map(|entry| {
                let (name, value) = entry.split_once(':')?;
                name.trim()
                    .eq_ignore_ascii_case("AWS_SESSION_TOKEN")
                    .then(|| value.to_string())
            })
        }) {
            found.insert("AWS_SESSION_TOKEN".into(), token);
        }
    }
    found
}

/// Puts saved secrets back where `text` still shows the mask, so editing a saved
/// connection never needs a password on screen. A value typed over the mask is
/// kept as typed. `original` is the saved text (any engine).
pub fn unmask(text: &str, original: &str) -> String {
    if !text.contains(MASK) {
        return text.to_string();
    }
    let secrets = secrets(original);
    let masked = regex::escape(MASK);
    let mut result = text.to_string();
    for key in SECRET_KEYS {
        let Some(secret) = secrets.get(key) else {
            continue;
        };
        for place in Place::all_for(key) {
            let value = if *place == Place::KeyValue {
                format!("'{}'", secret.replace('\\', "\\\\").replace('\'', "\\'"))
            } else {
                encode(secret)
            };
            result = place
                .pattern(key, &masked)
                .replace_all(&result, |c: &Captures| format!("{}{value}{}", &c[1], &c[3]))
                .into_owned();
        }
    }
    if let Some(token) = secrets.get("AWS_SESSION_TOKEN") {
        let value = encode(token);
        result = aws_token_pattern(&masked)
            .replace_all(&result, |c: &Captures| format!("{}{value}", &c[1]))
            .into_owned();
    }
    result
}

/// A Postgres URL from form fields. Empty fields are left out. With
/// `mask_password`, the password shows as the mask, for the screen.
#[allow(clippy::too_many_arguments)]
pub fn compose(
    scheme: &str,
    host: &str,
    port: &str,
    database: &str,
    user: &str,
    password: &str,
    sslmode: &str,
    mask_password: bool,
) -> String {
    let mut auth = encode(user);
    if !password.is_empty() {
        auth.push(':');
        auth.push_str(&if mask_password {
            MASK.to_string()
        } else {
            encode(password)
        });
    }
    if !auth.is_empty() {
        auth.push('@');
    }
    let port = port.trim();
    let host_part = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    let mut url = format!("{scheme}://{auth}{host_part}");
    if !port.is_empty() {
        url.push(':');
        url.push_str(port);
    }
    if !database.is_empty() {
        url.push('/');
        url.push_str(&encode(database));
    }
    if !sslmode.is_empty() {
        url.push_str("?sslmode=");
        url.push_str(sslmode);
    }
    url
}

// MARK: - Any scheme://

/// One host of a URL; port is None when not given.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct UrlHost {
    pub name: String,
    pub port: Option<u16>,
}

/// The parts of any scheme://user:password@host:port/path?query URL, decoded.
/// Postgres has its own parser (ConnectionUrl); the other engines start here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlParts {
    /// Lowercased, without "jdbc:" or a "+driver" suffix: "mysql", "redis", "mongodb+srv".
    pub scheme: String,
    pub user: Option<String>,
    pub password: Option<String>,
    /// Every host:port pair.
    pub hosts: Vec<UrlHost>,
    /// The path without its leading "/": the database name, a Redis index, a file.
    pub path: String,
    /// Query parameters, keys as written.
    pub query: BTreeMap<String, String>,
}

impl UrlParts {
    /// None when the text is not scheme://… (after cleaning an .env line or quotes).
    pub fn new(text: &str) -> Option<Self> {
        static HEAD: OnceLock<Regex> = OnceLock::new();
        let cleaned = clean(text);
        let m = re(&HEAD, r"^(?i)(jdbc:)?([a-z][a-z0-9.-]*)(\+[a-z0-9_]+)?://").find(&cleaned)?;
        let head = m.as_str().to_lowercase();
        let mut scheme = head.replace("jdbc:", "").replace("://", "");
        if !scheme.starts_with("mongodb+srv") {
            if let Some(plus) = scheme.find('+') {
                scheme.truncate(plus);
            }
        }
        let mut values = parse_url(&cleaned[m.end()..]);
        let names: Vec<String> = values
            .get("host")
            .map(|h| h.split(',').map(String::from).collect())
            .unwrap_or_else(|| vec![String::new()]);
        let ports: Vec<String> = values
            .get("port")
            .map(|p| p.split(',').map(String::from).collect())
            .unwrap_or_default();
        let single = names.len() == 1;
        let hosts = names
            .iter()
            .enumerate()
            .filter(|(_, name)| !(name.is_empty() && single))
            .map(|(i, name)| UrlHost {
                name: name.clone(),
                port: ports.get(i).and_then(|p| p.parse().ok()),
            })
            .collect();
        let user = values.remove("user");
        let password = values.remove("password");
        let path = values.remove("dbname").unwrap_or_default();
        values.remove("host");
        values.remove("port");
        Some(Self {
            scheme,
            user,
            password,
            hosts,
            path,
            query: values,
        })
    }

    pub fn first_host(&self) -> Option<&UrlHost> {
        self.hosts.first()
    }
}

// MARK: - SQLite

/// A SQLite file, from a path or a sqlite:/file: URL. Port of `SQLiteLocation`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SqliteLocation {
    /// The cleaned text that was pasted.
    pub raw: String,
    /// The absolute path, with ~ expanded.
    pub path: String,
}

const SQLITE_EXTENSIONS: [&str; 4] = ["db", "db3", "sqlite", "sqlite3"];
/// Longest first, so "sqlite3://" is not read as "sqlite3:" plus "//".
const SQLITE_SCHEMES: [&str; 6] = [
    "sqlite3://",
    "sqlite://",
    "sqlite3:",
    "sqlite:",
    "file://",
    "file:",
];

impl SqliteLocation {
    pub fn new(text: &str) -> Result<Self, DbError> {
        let cleaned = strip_env_name(&clean(text));
        if cleaned.is_empty() {
            return Err(not_sqlite());
        }
        let path = resolve(&cleaned)?;
        Ok(Self { raw: cleaned, path })
    }

    /// The file name, "app.db", for the sidebar.
    pub fn suggested_name(&self) -> String {
        Path::new(&self.path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.clone())
    }
}

fn not_sqlite() -> DbError {
    DbError::Message("That doesn't look like a SQLite file. Use a path ending in .db, .sqlite or .sqlite3, or sqlite:///path/to/file.db.".into())
}

fn starts_like_path(text: &str) -> bool {
    text.starts_with('/')
        || text.starts_with('~')
        || text.starts_with('.')
        || is_windows_absolute(text)
}

/// `DB_PATH=/x/app.db` and `DB=~/app.db`: clean() only strips the name before a URL.
fn strip_env_name(text: &str) -> String {
    static ENV: OnceLock<Regex> = OnceLock::new();
    let Some(m) = re(&ENV, r"^[A-Za-z_][A-Za-z0-9_]*\s*=\s*").find(text) else {
        return text.to_string();
    };
    let rest = text[m.end()..].trim_matches(|c| c == '"' || c == '\'' || c == ' ' || c == '\t');
    if starts_like_path(rest) {
        rest.to_string()
    } else {
        text.to_string()
    }
}

fn resolve(text: &str) -> Result<String, DbError> {
    let lower = text.to_lowercase();
    let Some(scheme) = SQLITE_SCHEMES.iter().find(|s| lower.starts_with(*s)) else {
        let ext = Path::new(text)
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if !starts_like_path(text) || !SQLITE_EXTENSIONS.contains(&ext.as_str()) {
            return Err(not_sqlite());
        }
        return Ok(absolute(text));
    };
    let mut rest = text[scheme.len()..].to_string();
    if let Some(cut) = rest.find(['?', '#']) {
        rest.truncate(cut);
    }
    if scheme.starts_with("file") {
        rest = decode(&rest);
        // file://localhost/path: the host is dropped.
        if *scheme == "file://" && !rest.starts_with('/') {
            if let Some(slash) = rest.find('/') {
                if ["", "localhost"].contains(&rest[..slash].to_lowercase().as_str()) {
                    rest = rest[slash..].to_string();
                }
            }
        }
    }
    // sqlite:///C:/data/app.db names a Windows drive.
    if rest.starts_with('/') && is_windows_absolute(&rest[1..]) {
        rest.remove(0);
    }
    if rest.is_empty() || rest == "/" {
        return Err(not_sqlite());
    }
    Ok(absolute(&rest))
}

fn home() -> PathBuf {
    directories::BaseDirs::new()
        .map(|d| d.home_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// ~ expanded; a relative path is relative to the home folder (an app has no
/// useful working directory). `.` and `..` are resolved without touching the disk.
fn absolute(text: &str) -> String {
    let path = if text == "~" {
        home()
    } else if let Some(rest) = text.strip_prefix("~/").or_else(|| text.strip_prefix("~\\")) {
        home().join(rest)
    } else if text.starts_with('/') || is_windows_absolute(text) {
        PathBuf::from(text)
    } else {
        home().join(text)
    };
    let mut normal = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                normal.pop();
            }
            other => normal.push(other),
        }
    }
    normal.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_full_urls() {
        let url = ConnectionUrl::new(
            "  postgresql://ana:s%40nha@db.example.com:6543/loja?sslmode=require ",
        )
        .unwrap();
        assert_eq!(url.host(), "db.example.com");
        assert_eq!(url.port(), Some(6543));
        assert_eq!(url.user(), Some("ana"));
        assert_eq!(url.password(), Some("s@nha"));
        assert_eq!(url.database(), "loja");
        assert_eq!(url.suggested_name(), "loja @ db.example.com");
        assert!(!url.redacted().contains("s%40nha"));
        assert_eq!(
            url.for_libpq(),
            "host='db.example.com' port='6543' dbname='loja' user='ana' password='s@nha' sslmode='require' connect_timeout='10'"
        );
    }

    #[test]
    fn rejects_other_input() {
        assert_eq!(ConnectionUrl::new("   "), Err(UrlProblem::Empty));
        assert_eq!(
            ConnectionUrl::new("mysql://a@b/c"),
            Err(UrlProblem::NotPostgres)
        );
        assert_eq!(ConnectionUrl::new("só texto"), Err(UrlProblem::NotPostgres));
    }

    #[test]
    fn database_defaults_to_user() {
        assert_eq!(
            ConnectionUrl::new("postgres://ana@localhost")
                .unwrap()
                .database(),
            "ana"
        );
    }

    #[test]
    fn pasted_formats() {
        type Case = (
            &'static str,
            &'static str,
            Option<u16>,
            &'static str,
            Option<&'static str>,
            Option<&'static str>,
        );
        let cases: [Case; 12] = [
            ("jdbc:postgresql://db.x.com:5433/loja?user=ana&password=pw&ssl=true", "db.x.com", Some(5433), "loja", Some("ana"), Some("require")),
            ("postgresql+psycopg2://ana:pw@db.x.com/loja", "db.x.com", None, "loja", Some("ana"), None),
            ("postgres+asyncpg://ana@db.x.com/loja", "db.x.com", None, "loja", Some("ana"), None),
            ("DATABASE_URL=\"postgresql://ana:pw@db.x.com:5432/loja?schema=public\"", "db.x.com", Some(5432), "loja", Some("ana"), None),
            ("export DATABASE_URL='postgres://ana@db.x.com/loja?sslmode=no-verify'", "db.x.com", None, "loja", Some("ana"), Some("require")),
            ("postgresql://ana@h1.x.com:5432,h2.x.com:5433/loja?target_session_attrs=read-write", "h1.x.com", Some(5432), "loja", Some("ana"), None),
            ("postgresql:///loja?host=/var/run/postgresql", "localhost", None, "loja", None, None),
            ("postgresql://%2Fvar%2Frun%2Fpostgresql/loja", "localhost", None, "loja", None, None),
            ("postgresql://ana@[::1]:5432/loja", "::1", Some(5432), "loja", Some("ana"), None),
            ("postgres://ana:pw@db.x.com/loja?connection_limit=5&pgbouncer=true", "db.x.com", None, "loja", Some("ana"), None),
            ("host=db.x.com port=5432 dbname=loja user=ana password='it\\'s secret' sslmode=require", "db.x.com", Some(5432), "loja", Some("ana"), Some("require")),
            ("postgresql://ana:pw@db.x.com/loja?username=outra&database=outro", "db.x.com", None, "loja", Some("ana"), None),
        ];
        for (text, host, port, database, user, sslmode) in cases {
            let url = ConnectionUrl::new(text).unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!(url.host(), host, "{text}");
            assert_eq!(url.port(), port, "{text}");
            assert_eq!(url.database(), database, "{text}");
            assert_eq!(url.user(), user, "{text}");
            assert_eq!(url.sslmode(), sslmode, "{text}");
            for bad in [
                "schema",
                "connection_limit",
                "pgbouncer",
                "ssl=",
                "username",
                "database=",
            ] {
                assert!(
                    !url.for_libpq().contains(bad),
                    "{bad} in {}",
                    url.for_libpq()
                );
            }
        }
    }

    #[test]
    fn driver_options_become_libpq_ones() {
        let prisma = ConnectionUrl::new("postgresql://ana@db.x.com/loja?schema=vendas").unwrap();
        assert_eq!(
            prisma.settings.get("options").map(String::as_str),
            Some("-c search_path=vendas")
        );
        let kv = ConnectionUrl::new("host=db.x.com dbname=loja password='it\\'s secret'").unwrap();
        assert_eq!(kv.password(), Some("it's secret"));
        assert!(kv.for_libpq().contains("password='it\\'s secret'"));
        assert!(!kv.redacted().contains("secret"));
        let hosts = ConnectionUrl::new("postgresql://h1:5432,h2:5433/loja").unwrap();
        assert_eq!(hosts.settings["host"], "h1,h2");
        assert_eq!(hosts.settings["port"], "5432,5433");
    }

    #[test]
    fn composes_urls_from_fields() {
        let text = compose(
            "postgresql",
            "db.x.com",
            "5433",
            "minha loja",
            "ana",
            "p@ss:w/rd",
            "require",
            false,
        );
        assert_eq!(
            text,
            "postgresql://ana:p%40ss%3Aw%2Frd@db.x.com:5433/minha%20loja?sslmode=require"
        );
        let url = ConnectionUrl::new(&text).unwrap();
        assert_eq!(url.password(), Some("p@ss:w/rd"));
        assert_eq!(url.database(), "minha loja");
        assert_eq!(url.port(), Some(5433));
    }

    #[test]
    fn tunnel_settings() {
        let url =
            ConnectionUrl::new("postgres://ana:pw@localhost:5433/loja?sslmode=disable").unwrap();
        assert_eq!(
            url.for_libpq_through_local_port(61000),
            "host='127.0.0.1' port='61000' dbname='loja' user='ana' password='pw' sslmode='disable' connect_timeout='10'"
        );
        assert!(!url.conninfo_without_password(None).contains("pw"));
    }

    #[test]
    fn quotes_identifiers() {
        assert_eq!(quote_ident("my\"table"), "\"my\"\"table\"");
    }

    #[test]
    fn unmasks_passwords() {
        for original in [
            "postgresql://ana:p%40ss@db.x.com:5432/loja?sslmode=require",
            "postgres://ana@db.x.com/loja?password=p%40ss",
            "host=db.x.com dbname=loja user=ana password='p@ss'",
        ] {
            let saved = ConnectionUrl::new(original).unwrap();
            let shown = saved.redacted();
            assert!(
                !shown.contains("p@ss") && !shown.contains("p%40ss"),
                "{shown}"
            );
            let back = ConnectionUrl::new(&unmask(&shown, original)).unwrap();
            assert_eq!(back.password(), Some("p@ss"), "{original}");
            let edited = shown.replace("loja", "outra");
            let back = ConnectionUrl::new(&unmask(&edited, original)).unwrap();
            assert_eq!(back.password(), Some("p@ss"));
            assert_eq!(back.database(), "outra");
            let typed = edited.replace(MASK, "nova");
            assert_eq!(
                ConnectionUrl::new(&unmask(&typed, original))
                    .unwrap()
                    .password(),
                Some("nova")
            );
        }
    }

    #[test]
    fn redacts_every_secret() {
        for (text, secret) in [
            ("postgres://u:p@ss@db.x.com/loja", "p@ss"),
            ("postgres://u@db.x.com/loja?PASSWORD=segredo", "segredo"),
            (
                "postgres://u:pw@db.x.com/loja?sslpassword=chave&sslmode=require",
                "chave",
            ),
            ("host=db user=u Password=segredo dbname=loja", "segredo"),
        ] {
            let url = ConnectionUrl::new(text).unwrap();
            assert!(!url.redacted().contains(secret), "{}", url.redacted());
            let back = ConnectionUrl::new(&unmask(&url.redacted(), text)).unwrap();
            assert_eq!(back.settings, url.settings, "{text}");
        }
    }

    #[test]
    fn redacts_other_engines() {
        assert_eq!(
            redact("mongodb+srv://ana:secret@c.x.net/shop"),
            "mongodb+srv://ana:•••@c.x.net/shop"
        );
        assert_eq!(
            redact("redis://:pw@cache:6379/0"),
            "redis://:•••@cache:6379/0"
        );
        for (text, secret) in [
            (
                "mongodb://u@h/db?tlsCertificateKeyFilePassword=PW3&tls=true",
                "PW3",
            ),
            ("mongodb://u@h/db?sslPEMKeyPassword=PW4", "PW4"),
            ("mongodb://u@h/db?proxyHost=p&proxyPassword=PW5", "PW5"),
            (
                "mongodb://h/db?authMechanism=MONGODB-AWS&authMechanismProperties=AWS_SESSION_TOKEN:Zq9%2Bx",
                "Zq9",
            ),
            (
                "mongodb://h/db?authMechanismProperties=SERVICE_NAME:m,AWS_SESSION_TOKEN%3AZq9&x=1",
                "Zq9",
            ),
        ] {
            let shown = redact(text);
            assert!(!shown.contains(secret), "{shown}");
            assert!(shown.contains(MASK), "{shown}");
            let back = unmask(&shown, text);
            assert_eq!(
                secrets(&back),
                secrets(text),
                "{text} came back as {back}"
            );
            assert!(back.contains(secret), "{back}");
        }
        let masked = redact("mysql://root:p%40ss@127.0.0.1:3306/shop");
        assert_eq!(
            unmask(&masked, "mysql://root:p%40ss@127.0.0.1:3306/shop"),
            "mysql://root:p%40ss@127.0.0.1:3306/shop"
        );
    }

    #[test]
    fn url_parts() {
        let parts =
            UrlParts::new("jdbc:mysql+pymysql://ana:p%40ss@h1:3307,h2/shop?ssl-mode=REQUIRED")
                .unwrap();
        assert_eq!(parts.scheme, "mysql");
        assert_eq!(parts.user.as_deref(), Some("ana"));
        assert_eq!(parts.password.as_deref(), Some("p@ss"));
        assert_eq!(
            parts.hosts,
            vec![
                UrlHost {
                    name: "h1".into(),
                    port: Some(3307)
                },
                UrlHost {
                    name: "h2".into(),
                    port: None
                }
            ]
        );
        assert_eq!(parts.path, "shop");
        assert_eq!(
            parts.query.get("ssl-mode").map(String::as_str),
            Some("REQUIRED")
        );
        assert_eq!(
            UrlParts::new("mongodb+srv://c.x.net/app").unwrap().scheme,
            "mongodb+srv"
        );
        assert!(UrlParts::new("redis:///0").unwrap().hosts.is_empty());
        assert!(UrlParts::new("host=x").is_none());
    }

    #[test]
    fn cleans_env_lines() {
        assert_eq!(
            clean("  DATABASE_URL=\"postgres://a@b/c\"\n"),
            "postgres://a@b/c"
        );
        assert_eq!(clean("export REDIS_URL='redis://x'"), "redis://x");
        assert_eq!(clean("host=a dbname=b"), "host=a dbname=b");
    }

    #[test]
    fn sqlite_locations() {
        let home = home();
        let loc = SqliteLocation::new("sqlite:///tmp/x/../app.db").unwrap();
        assert_eq!(loc.path, "/tmp/app.db");
        assert_eq!(loc.suggested_name(), "app.db");
        assert_eq!(
            SqliteLocation::new("file://localhost/tmp/a%20b.db")
                .unwrap()
                .path,
            "/tmp/a b.db"
        );
        assert_eq!(
            SqliteLocation::new("~/data.sqlite3").unwrap().path,
            home.join("data.sqlite3").to_string_lossy()
        );
        assert_eq!(
            SqliteLocation::new("DB_PATH=./app.db").unwrap().path,
            home.join("app.db").to_string_lossy()
        );
        assert!(SqliteLocation::new("/tmp/notes.txt").is_err());
        assert!(SqliteLocation::new("sqlite:").is_err());
        assert!(SqliteLocation::new("postgres://a@b/c").is_err());
    }
}
