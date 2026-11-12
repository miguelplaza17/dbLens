//! File > Import Connections from dbLens for Mac…: copies the saved
//! connections of the Swift app (dbLens for Mac up to 1.4, retired) into this app's store.
//!
//! What the Swift app keeps, in `~/Library/Application Support/dbLens/`:
//! - `dbLens.store`: SwiftData (a Core Data SQLite store). Table
//!   `ZSAVEDCONNECTION` (entity `SavedConnection`, Sources/dbLens/Models.swift):
//!   columns `Z<PROPERTY IN CAPITALS>`, UUIDs as 16-byte blobs, dates as
//!   seconds since 2001-01-01. Read from a private temporary copy, so the
//!   original (and its -wal/-shm) is never opened, locked or changed.
//! - `connections.enc`: `{ "UUID": "url" }` as JSON, sealed with AES-GCM in
//!   CryptoKit's combined layout (12-byte nonce, ciphertext, 16-byte tag).
//! - `connections.key`: JSON, Sources/DBLensKit/SecretBox.swift. Kind
//!   "software" holds the AES key itself (opened here); kind "secure-enclave"
//!   needs this Mac's Secure Enclave, through `swift/SecureEnclaveOpen.swift`.
//! - Older versions: a plain `connections.json` instead of the sealed file.
//! - Preferences (a plist; `DBLENS_SWIFT_DEFAULTS` names a file instead): folders
//!   (`connectionFolders`), SQL history (`sqlHistory.<uuid>`, JSON as data) and the
//!   settings with a counterpart here (accent, SQL panel, confirm delete, menu
//!   bar, agents managing connections).
//!
//! Nothing there is written. This app's own store is a different folder
//! (`Store::default_dir`), checked before importing.

// The readers are platform independent and unit-tested everywhere; only the
// command uses them on macOS.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::Engine as _;
use rusqlite::types::ValueRef;
use serde::{Deserialize, Serialize};

use crate::history::Entry as HistoryEntry;
use crate::session::{DbError, DbResult};
use crate::ssh::SshConfig;
use crate::store::{ConnectionDraft, Store};

/// The released Swift app's bundle id (Info.plist.in); its preferences domain.
pub const SWIFT_BUNDLE_ID: &str = "io.github.miguelplaza17.dblens";
const STORE_FILE: &str = "dbLens.store";
const SEALED_FILE: &str = "connections.enc";
const KEY_FILE: &str = "connections.key";
const PLAIN_FILE: &str = "connections.json";
/// 2001-01-01T00:00:00Z, Core Data's reference date, in Unix seconds.
const CORE_DATA_EPOCH: f64 = 978_307_200.0;

/// A saved connection as the Swift app keeps it (without its URL).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SwiftConnection {
    /// Upper case, as Swift's `uuidString`: the key in connections.enc.
    pub uuid: String,
    pub name: String,
    /// RFC 3339.
    pub created_at: Option<String>,
    pub last_used: Option<String>,
    pub ssh: Option<SshConfig>,
    pub sort_index: i64,
    /// A `SwiftFolder::id`, or "" at the top level.
    pub folder_id: String,
    /// "Let Agents Write", "hidden from agents", the MCP tools turned off and
    /// the notes for agents (Models.swift `agents*`).
    pub agents_can_write: bool,
    pub agents_hidden: bool,
    pub agents_blocked_tools: Vec<String>,
    pub agents_notes: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SwiftFolder {
    pub id: String,
    pub name: String,
    #[serde(default = "yes")]
    pub expanded: bool,
    /// The Swift folder it sits in (upper case), None at the top level.
    #[serde(default, rename = "parentID")]
    pub parent_id: Option<String>,
}

fn yes() -> bool {
    true
}

/// The Swift app's preferences that have a counterpart here; None when unset.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SwiftSettings {
    /// `appearance.accent` (a theme id).
    pub accent: Option<String>,
    /// `keepInMenuBar`.
    pub keep_in_menu_bar: Option<bool>,
    /// `sqlPanel.shown`.
    pub show_sql_panel: Option<bool>,
    /// `confirmDelete`.
    pub confirm_delete: Option<bool>,
    /// `agentsManageConnections`.
    pub agents_manage_connections: Option<bool>,
}

/// Everything read from the Swift app.
#[derive(Debug, Default)]
pub struct SwiftApp {
    /// Upper-case connection uuid → its SQL history, newest first.
    pub history: BTreeMap<String, Vec<HistoryEntry>>,
    pub settings: SwiftSettings,
    pub connections: Vec<SwiftConnection>,
    /// Upper-case uuid → URL.
    pub urls: BTreeMap<String, String>,
    pub folders: Vec<SwiftFolder>,
}

/// What the import did. Errors name the connection, never its URL.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub imported: usize,
    /// Already here: same URL and name.
    pub skipped: usize,
    /// Folders made here.
    pub folders: usize,
    /// SQL history entries added.
    pub history: usize,
    /// App settings taken over (those still at their defaults here).
    pub settings: usize,
    pub errors: Vec<String>,
}

fn problem(text: impl std::fmt::Display) -> DbError {
    DbError::Message(format!("dbLens for Mac: {text}"))
}

// ---------------------------------------------------------------- the store

/// The Swift app's saved connections, from a temporary copy of `store`.
pub fn read_connections(store: &Path) -> DbResult<Vec<SwiftConnection>> {
    if !store.is_file() {
        return Err(problem(format!(
            "no saved connections at {}",
            store.display()
        )));
    }
    let copy = TempCopy::of(store)?;
    let db = rusqlite::Connection::open(copy.path()).map_err(problem)?;
    read_connections_from(&db)
}

/// Reads the `SavedConnection` rows. Columns a store version lacks read as
/// the model's defaults, so older stores import too.
pub fn read_connections_from(db: &rusqlite::Connection) -> DbResult<Vec<SwiftConnection>> {
    // Core Data names an entity's table "Z" + its name in capitals.
    let table = db
        .query_row(
            "select Z_NAME from Z_PRIMARYKEY where Z_NAME = 'SavedConnection'",
            [],
            |row| row.get::<_, String>(0),
        )
        .map(|name| format!("Z{}", name.to_uppercase()))
        .unwrap_or_else(|_| "ZSAVEDCONNECTION".to_string());
    let mut statement = db
        .prepare(&format!("select * from \"{table}\""))
        .map_err(|_| problem("its store has no saved connections table"))?;
    let columns: HashMap<String, usize> = statement
        .column_names()
        .enumerate()
        .map(|(i, name)| (name.to_uppercase(), i))
        .collect();
    let mut rows = statement.query([]).map_err(problem)?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().map_err(problem)? {
        let get = |name: &str| columns.get(name).and_then(|&i| row.get_ref(i).ok());
        let text = |name: &str| get(name).and_then(value_text).unwrap_or_default();
        let int = |name: &str| get(name).and_then(value_int).unwrap_or(0);
        let date = |name: &str| get(name).and_then(value_date);
        let Some(uuid) = get("ZUUID").and_then(value_uuid) else {
            continue; // never saved a URL under an id: nothing to import
        };
        let host = text("ZSSHHOST");
        let ssh = (!host.trim().is_empty()).then(|| SshConfig {
            host: host.trim().to_string(),
            port: u16::try_from(int("ZSSHPORT")).ok().filter(|&p| p > 0),
            user: text("ZSSHUSER"),
            key_path: text("ZSSHKEY"),
        });
        out.push(SwiftConnection {
            uuid,
            name: text("ZNAME"),
            created_at: date("ZCREATEDAT"),
            last_used: date("ZLASTUSED"),
            ssh,
            sort_index: int("ZSORTINDEX"),
            folder_id: text("ZFOLDERID").to_uppercase(),
            agents_can_write: int("ZAGENTSCANWRITE") != 0,
            agents_hidden: int("ZAGENTSHIDDEN") != 0,
            agents_blocked_tools: text("ZAGENTSBLOCKEDTOOLS")
                .split(',')
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(String::from)
                .collect(),
            agents_notes: text("ZAGENTSNOTES"),
        });
    }
    Ok(out)
}

fn value_text(value: ValueRef<'_>) -> Option<String> {
    match value {
        ValueRef::Text(t) => Some(String::from_utf8_lossy(t).into_owned()),
        _ => None,
    }
}

fn value_int(value: ValueRef<'_>) -> Option<i64> {
    match value {
        ValueRef::Integer(i) => Some(i),
        ValueRef::Real(r) => Some(r as i64),
        _ => None,
    }
}

/// Core Data dates: seconds (REAL) since 2001-01-01, as RFC 3339.
fn value_date(value: ValueRef<'_>) -> Option<String> {
    let seconds = match value {
        ValueRef::Real(r) => r,
        ValueRef::Integer(i) => i as f64,
        _ => return None,
    };
    core_data_seconds(seconds)
}

fn core_data_seconds(seconds: f64) -> Option<String> {
    chrono::DateTime::from_timestamp((seconds + CORE_DATA_EPOCH).floor() as i64, 0)
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

/// A UUID as a 16-byte blob (or text), in Swift's upper-case form.
fn value_uuid(value: ValueRef<'_>) -> Option<String> {
    let uuid = match value {
        ValueRef::Blob(b) => uuid::Uuid::from_slice(b).ok()?,
        ValueRef::Text(t) => uuid::Uuid::parse_str(std::str::from_utf8(t).ok()?).ok()?,
        _ => return None,
    };
    Some(uuid.hyphenated().to_string().to_uppercase())
}

/// A copy of a SQLite file and its -wal/-shm in a private temporary folder,
/// removed on drop.
struct TempCopy {
    dir: PathBuf,
    file: PathBuf,
}

impl TempCopy {
    fn of(source: &Path) -> DbResult<TempCopy> {
        let dir = std::env::temp_dir().join(format!("dblens-import-{}", uuid::Uuid::new_v4()));
        #[cfg(unix)]
        let builder = {
            let mut builder = std::fs::DirBuilder::new();
            std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
            builder
        };
        #[cfg(not(unix))]
        let builder = std::fs::DirBuilder::new();
        builder.create(&dir).map_err(problem)?;
        let copy = TempCopy {
            file: dir.join(STORE_FILE),
            dir,
        };
        std::fs::copy(source, &copy.file).map_err(problem)?;
        for suffix in ["-wal", "-shm"] {
            let mut side = source.as_os_str().to_owned();
            side.push(suffix);
            let side = PathBuf::from(side);
            if side.is_file() {
                let mut target = copy.file.as_os_str().to_owned();
                target.push(suffix);
                std::fs::copy(&side, PathBuf::from(target)).map_err(problem)?;
            }
        }
        Ok(copy)
    }

    fn path(&self) -> &Path {
        &self.file
    }
}

impl Drop for TempCopy {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

// ---------------------------------------------------------------- the URLs

/// connections.key, as SecretBox.swift writes it (Data is base64).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KeyFile {
    kind: String,
    #[serde(default)]
    enclave_key: Option<String>,
    #[serde(default)]
    peer_public_key: Option<String>,
    #[serde(default)]
    software_key: Option<String>,
}

fn base64_field(value: Option<&str>, what: &str) -> DbResult<Vec<u8>> {
    value
        .and_then(|v| base64::engine::general_purpose::STANDARD.decode(v).ok())
        .ok_or_else(|| problem(format!("connections.key has no usable {what}")))
}

/// The saved URLs in `dir`: connections.enc opened with connections.key, or
/// the plain connections.json of older versions; empty when there is neither.
pub fn read_urls(dir: &Path) -> DbResult<BTreeMap<String, String>> {
    let sealed_path = dir.join(SEALED_FILE);
    if sealed_path.is_file() {
        let sealed = std::fs::read(&sealed_path).map_err(problem)?;
        let key_text = std::fs::read(dir.join(KEY_FILE))
            .map_err(|_| problem("connections.key is missing, so its URLs can't be opened"))?;
        let key: KeyFile =
            serde_json::from_slice(&key_text).map_err(|_| problem("connections.key is damaged"))?;
        let plain = match key.kind.as_str() {
            "secure-enclave" => open_with_enclave(
                &base64_field(key.enclave_key.as_deref(), "Secure Enclave key")?,
                &base64_field(key.peer_public_key.as_deref(), "public key")?,
                &sealed,
            )?,
            _ => open_software(&base64_field(key.software_key.as_deref(), "key")?, &sealed)?,
        };
        return parse_urls(&plain);
    }
    match std::fs::read(dir.join(PLAIN_FILE)) {
        Ok(plain) => parse_urls(&plain),
        Err(_) => Ok(BTreeMap::new()),
    }
}

/// `{ "UUID": "url" }`, keys in upper case.
pub fn parse_urls(plain: &[u8]) -> DbResult<BTreeMap<String, String>> {
    let all: BTreeMap<String, String> = serde_json::from_slice(plain)
        .map_err(|_| problem("its saved URLs aren't the expected JSON"))?;
    Ok(all
        .into_iter()
        .map(|(id, url)| (id.to_uppercase(), url))
        .collect())
}

/// Kind "software": the 32-byte key itself; CryptoKit's combined box.
pub fn open_software(key: &[u8], sealed: &[u8]) -> DbResult<Vec<u8>> {
    if key.len() != 32 {
        return Err(problem("connections.key has no usable key"));
    }
    if sealed.len() < 12 + 16 {
        return Err(problem("connections.enc is damaged"));
    }
    let (nonce, body) = sealed.split_at(12);
    Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key))
        .decrypt(Nonce::from_slice(nonce), body)
        .map_err(|_| problem("connections.enc doesn't open with connections.key"))
}

#[cfg(target_os = "macos")]
fn open_with_enclave(enclave_key: &[u8], peer: &[u8], sealed: &[u8]) -> DbResult<Vec<u8>> {
    extern "C" {
        // swift/SecureEnclaveOpen.swift; Swift's Int is isize.
        fn dblens_secure_enclave_open(
            enclave_key: *const u8,
            enclave_key_count: isize,
            peer_public_key: *const u8,
            peer_public_key_count: isize,
            sealed: *const u8,
            sealed_count: isize,
            out: *mut u8,
            out_count: isize,
        ) -> i32;
    }
    if sealed.len() < 12 + 16 {
        return Err(problem("connections.enc is damaged"));
    }
    let mut out = vec![0u8; sealed.len() - 12 - 16];
    // SAFETY: every pointer comes with its slice's length, the Swift side
    // only reads the inputs and writes at most `out.len()` bytes into `out`,
    // and keeps none of them after it returns.
    let status = unsafe {
        dblens_secure_enclave_open(
            enclave_key.as_ptr(),
            enclave_key.len() as isize,
            peer.as_ptr(),
            peer.len() as isize,
            sealed.as_ptr(),
            sealed.len() as isize,
            out.as_mut_ptr(),
            out.len() as isize,
        )
    };
    match status {
        0 => Ok(out),
        2 => Err(problem("its key is in a Secure Enclave, and this Mac has none")),
        3 => Err(problem(
            "its key belongs to another Mac's Secure Enclave (or another user), so its URLs can't be opened",
        )),
        4 | 5 => Err(problem("connections.enc doesn't open with this Mac's key")),
        _ => Err(problem("connections.key is damaged")),
    }
}

#[cfg(not(target_os = "macos"))]
fn open_with_enclave(_: &[u8], _: &[u8], _: &[u8]) -> DbResult<Vec<u8>> {
    Err(problem(
        "its key is in a Mac's Secure Enclave; import on that Mac",
    ))
}

// ---------------------------------------------------------------- folders

/// The folders from the Swift app's preferences (a plist, XML or binary):
/// key `connectionFolders`, a JSON array as data. Empty when missing.
pub fn parse_folders(plist_bytes: &[u8]) -> Vec<SwiftFolder> {
    let Ok(value) = plist::Value::from_reader(std::io::Cursor::new(plist_bytes)) else {
        return Vec::new();
    };
    let Some(data) = value
        .as_dictionary()
        .and_then(|d| d.get("connectionFolders"))
        .and_then(plist::Value::as_data)
    else {
        return Vec::new();
    };
    serde_json::from_slice::<Vec<SwiftFolder>>(data)
        .unwrap_or_default()
        .into_iter()
        .map(|f| SwiftFolder {
            id: f.id.to_uppercase(),
            parent_id: f.parent_id.map(|p| p.to_uppercase()),
            ..f
        })
        .collect()
}

fn preferences_dictionary(plist_bytes: &[u8]) -> Option<plist::Dictionary> {
    plist::Value::from_reader(std::io::Cursor::new(plist_bytes))
        .ok()?
        .into_dictionary()
}

/// Swift's `HistoryEntry` (Models.swift), JSON in user defaults: `date` is
/// Codable's default, seconds since 2001 (an RFC 3339 string reads too).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SwiftHistoryEntry {
    sql: String,
    #[serde(default)]
    date: Option<serde_json::Value>,
    #[serde(default)]
    failed: bool,
    #[serde(default)]
    via_agent: Option<bool>,
}

/// The SQL history from the preferences: keys `sqlHistory.<uuid>`, each a JSON
/// array as data. Entries with no text are dropped.
pub fn parse_history(plist_bytes: &[u8]) -> BTreeMap<String, Vec<HistoryEntry>> {
    let mut out = BTreeMap::new();
    let Some(dict) = preferences_dictionary(plist_bytes) else {
        return out;
    };
    for (key, value) in dict.iter() {
        let Some(id) = key.strip_prefix("sqlHistory.") else {
            continue;
        };
        let Some(data) = value.as_data() else {
            continue;
        };
        let Ok(entries) = serde_json::from_slice::<Vec<SwiftHistoryEntry>>(data) else {
            continue;
        };
        let list: Vec<HistoryEntry> = entries
            .into_iter()
            .filter(|e| !e.sql.trim().is_empty())
            .map(|e| HistoryEntry {
                date: match &e.date {
                    Some(serde_json::Value::Number(n)) => n.as_f64().and_then(core_data_seconds),
                    Some(serde_json::Value::String(s)) => {
                        chrono::DateTime::parse_from_rfc3339(s).ok().map(|d| {
                            d.with_timezone(&chrono::Utc)
                                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
                        })
                    }
                    _ => None,
                }
                .unwrap_or_else(|| core_data_seconds(0.0).unwrap_or_default()),
                sql: e.sql,
                failed: e.failed,
                via_agent: e.via_agent.unwrap_or(false),
            })
            .collect();
        if !list.is_empty() {
            out.insert(id.to_uppercase(), list);
        }
    }
    out
}

/// The app settings from the preferences.
pub fn parse_settings(plist_bytes: &[u8]) -> SwiftSettings {
    let Some(dict) = preferences_dictionary(plist_bytes) else {
        return SwiftSettings::default();
    };
    let flag = |key: &str| dict.get(key).and_then(plist::Value::as_boolean);
    SwiftSettings {
        accent: dict
            .get("appearance.accent")
            .and_then(plist::Value::as_string)
            .map(String::from),
        keep_in_menu_bar: flag("keepInMenuBar"),
        show_sql_panel: flag("sqlPanel.shown"),
        confirm_delete: flag("confirmDelete"),
        agents_manage_connections: flag("agentsManageConnections"),
    }
}

/// The settings to change in `current` (a JSON patch for `Settings::patched`):
/// only those still at the defaults here, so a choice made in this app is
/// never overwritten. Accents Swift has under another name don't exist
/// (both apps share the ids); an unknown one is ignored.
pub fn settings_patch(
    swift: &SwiftSettings,
    current: &crate::settings::Settings,
) -> serde_json::Map<String, serde_json::Value> {
    use crate::settings::{Settings, ACCENTS};
    let defaults = Settings::default();
    let mut patch = serde_json::Map::new();
    if let Some(accent) = swift.accent.as_deref() {
        if ACCENTS.contains(&accent)
            && current.accent == defaults.accent
            && accent != defaults.accent
        {
            patch.insert("accent".into(), accent.into());
        }
    }
    let flags = [
        (
            "keepInTray",
            swift.keep_in_menu_bar,
            current.keep_in_tray,
            defaults.keep_in_tray,
        ),
        (
            "showSqlPanel",
            swift.show_sql_panel,
            current.show_sql_panel,
            defaults.show_sql_panel,
        ),
        (
            "confirmDelete",
            swift.confirm_delete,
            current.confirm_delete,
            defaults.confirm_delete,
        ),
    ];
    for (key, theirs, ours, default) in flags {
        if let Some(value) = theirs {
            if ours == default && value != default {
                patch.insert(key.into(), value.into());
            }
        }
    }
    patch
}

/// The preferences as cfprefsd has them now (`defaults export`, which sees
/// changes not yet written to disk), else the file in ~/Library/Preferences.
#[cfg(target_os = "macos")]
fn read_preferences() -> Option<Vec<u8>> {
    // Tests: a plist file stands in for the real preferences.
    if let Some(file) = std::env::var_os("DBLENS_SWIFT_DEFAULTS").filter(|f| !f.is_empty()) {
        return std::fs::read(file).ok();
    }
    let exported = std::process::Command::new("/usr/bin/defaults")
        .args(["export", SWIFT_BUNDLE_ID, "-"])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success() && !o.stdout.is_empty())
        .map(|o| o.stdout);
    exported.or_else(|| {
        let home = directories::BaseDirs::new()?.home_dir().to_path_buf();
        std::fs::read(home.join(format!("Library/Preferences/{SWIFT_BUNDLE_ID}.plist"))).ok()
    })
}

// ---------------------------------------------------------------- import

/// `~/Library/Application Support/dbLens`, or `$DBLENS_SWIFT_APP_DIR`.
pub fn swift_app_dir() -> DbResult<PathBuf> {
    if let Some(dir) = std::env::var_os("DBLENS_SWIFT_APP_DIR").filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    directories::BaseDirs::new()
        .map(|d| d.data_dir().join("dbLens"))
        .ok_or_else(|| problem("no home folder"))
}

/// Reads connections and URLs from `dir`, folders from `preferences`.
pub fn read_swift_app(dir: &Path, preferences: Option<&[u8]>) -> DbResult<SwiftApp> {
    Ok(SwiftApp {
        connections: read_connections(&dir.join(STORE_FILE))?,
        urls: read_urls(dir)?,
        folders: preferences.map(parse_folders).unwrap_or_default(),
        history: preferences.map(parse_history).unwrap_or_default(),
        settings: preferences.map(parse_settings).unwrap_or_default(),
    })
}

/// Adds the Swift app's connections to `store`, in its sidebar order, with
/// their folders (an existing folder of the same name is reused), SSH
/// settings and dates. A connection with the same URL and name is skipped,
/// so importing twice adds nothing.
pub fn apply(store: &mut Store, app: SwiftApp) -> ImportReport {
    apply_collect(store, &app).0
}

/// Swift connection uuid (upper case) → the id of its connection here, for
/// those imported now and those that were already here.
pub type LocalIds = HashMap<String, String>;

/// [`apply`], also saying which connection here each Swift one became (the
/// history is filed under it), and setting what is Swift-only per connection
/// (agent access, notes) on the ones made now.
pub fn apply_collect(store: &mut Store, app: &SwiftApp) -> (ImportReport, LocalIds) {
    let mut report = ImportReport::default();
    let mut ids = LocalIds::new();
    let snapshot = store.snapshot();
    let folders_before = snapshot.folders.len();
    let mut present: HashMap<(String, String), String> = snapshot
        .connections
        .iter()
        .filter_map(|c| {
            store
                .url(&c.id)
                .map(|u| ((u.to_string(), c.name.clone()), c.id.clone()))
        })
        .collect();
    let folder_order: HashMap<&str, usize> = app
        .folders
        .iter()
        .enumerate()
        .map(|(i, f)| (f.id.as_str(), i))
        .collect();
    let mut connections = app.connections.clone();
    // Top level first, then each folder in the Swift sidebar's order.
    connections.sort_by_key(|c| {
        let folder = match c.folder_id.as_str() {
            "" => 0,
            id => folder_order.get(id).map_or(usize::MAX, |i| i + 1),
        };
        (folder, c.sort_index, c.name.clone())
    });
    // Swift folder id → this store's folder id, made when first needed.
    let mut folders: HashMap<String, String> = HashMap::new();

    for c in connections {
        let label = if c.name.trim().is_empty() {
            "A connection without a name".to_string()
        } else {
            c.name.trim().to_string()
        };
        let Some(raw) = app.urls.get(&c.uuid) else {
            report.errors.push(format!(
                "{label}: its URL wasn't saved, so there is nothing to import"
            ));
            continue;
        };
        let url = crate::url::clean(raw);
        let name = match c.name.trim() {
            "" => crate::connector::summary(&url)
                .map(|s| s.suggested_name)
                .unwrap_or_default(),
            name => name.to_string(),
        };
        if let Some(id) = present.get(&(url.clone(), name.clone())) {
            ids.insert(c.uuid.clone(), id.clone());
            report.skipped += 1;
            continue;
        }
        let folder_id = match app.folders.iter().find(|f| f.id == c.folder_id) {
            None => String::new(),
            Some(folder) => match local_folder(store, &app.folders, folder, &mut folders, 0) {
                Ok(id) => id,
                Err(e) => {
                    report.errors.push(format!("{}: {e}", folder.name));
                    String::new()
                }
            },
        };
        let draft = ConnectionDraft {
            id: None,
            name: name.clone(),
            url: url.clone(),
            ssh: c.ssh.clone(),
            folder_id: Some(folder_id),
        };
        match store.save(draft) {
            Ok(saved) => {
                if let Err(e) =
                    store.set_dates(&saved.id, c.created_at.clone(), c.last_used.clone())
                {
                    report.errors.push(format!("{label}: {e}"));
                }
                if let Err(e) = apply_agent_settings(store, &saved.id, &c) {
                    report.errors.push(format!("{label}: {e}"));
                }
                ids.insert(c.uuid.clone(), saved.id.clone());
                present.insert((url, saved.name), saved.id);
                report.imported += 1;
            }
            Err(e) => report.errors.push(format!("{label}: {e}")),
        }
    }
    report.folders = store
        .snapshot()
        .folders
        .len()
        .saturating_sub(folders_before);
    if app.settings.agents_manage_connections == Some(true)
        && !store.snapshot().agents_manage_connections
        && store.set_agents_manage_connections(true).is_ok()
    {
        report.settings += 1;
    }
    (report, ids)
}

/// What Swift knew about agents and this connection; the defaults are left alone.
fn apply_agent_settings(store: &mut Store, id: &str, c: &SwiftConnection) -> DbResult<()> {
    if c.agents_can_write {
        store.set_agents_can_write(id, true)?;
    }
    if c.agents_hidden || !c.agents_blocked_tools.is_empty() {
        store.set_agents_access(id, c.agents_hidden, c.agents_blocked_tools.clone())?;
    }
    if !c.agents_notes.trim().is_empty() {
        store.set_agents_notes(id, &c.agents_notes)?;
    }
    Ok(())
}

/// The folder of that name and place in `store` (its Swift parents made
/// first), made with its expanded state when there is none. `made` maps
/// Swift ids to this store's ids.
fn local_folder(
    store: &mut Store,
    all: &[SwiftFolder],
    folder: &SwiftFolder,
    made: &mut HashMap<String, String>,
    depth: usize,
) -> DbResult<String> {
    if let Some(id) = made.get(&folder.id) {
        return Ok(id.clone());
    }
    // A parent that is missing (or a loop) puts the folder at the top level.
    let parent = match folder
        .parent_id
        .as_ref()
        .and_then(|p| all.iter().find(|f| &f.id == p))
    {
        Some(parent) if depth < 32 => local_folder(store, all, parent, made, depth + 1)?,
        _ => String::new(),
    };
    let id = local_folder_in(store, folder, &parent)?;
    made.insert(folder.id.clone(), id.clone());
    Ok(id)
}

fn local_folder_in(store: &mut Store, folder: &SwiftFolder, parent: &str) -> DbResult<String> {
    let name = folder.name.trim();
    if let Some(existing) = store
        .snapshot()
        .folders
        .iter()
        .find(|f| f.name == name && f.parent_id == parent)
    {
        return Ok(existing.id.clone());
    }
    let made = store.add_folder(name, parent)?;
    if !folder.expanded {
        store.set_folder_expanded(&made.id, false)?;
    }
    Ok(made.id)
}

/// Never read from (and so never later write next to) this app's own store.
fn check_distinct(swift_dir: &Path) -> DbResult<()> {
    let ours = Store::default_dir()?;
    let same = |a: &Path, b: &Path| {
        let a = a.canonicalize().unwrap_or_else(|_| a.to_path_buf());
        let b = b.canonicalize().unwrap_or_else(|_| b.to_path_buf());
        // macOS volumes are usually case-insensitive.
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    };
    if same(&ours, swift_dir) {
        return Err(problem("its folder is this app's own store folder"));
    }
    Ok(())
}

/// The whole import: connections and folders, then their SQL history and the
/// app settings (only those still at their defaults here).
#[cfg(target_os = "macos")]
async fn import_everything(
    app: &tauri::AppHandle,
    state: &crate::commands::AppState,
    history: &crate::history::HistoryState,
    settings: &crate::settings::SettingsState,
) -> DbResult<ImportReport> {
    use tauri::Emitter;
    let swift = tokio::task::spawn_blocking(|| {
        let dir = swift_app_dir()?;
        check_distinct(&dir)?;
        read_swift_app(&dir, read_preferences().as_deref())
    })
    .await
    .map_err(|e| DbError::Message(e.to_string()))??;
    let (mut report, ids) = state
        .with_store(|store| Ok(apply_collect(store, &swift)))
        .await?;
    // After the store's lock is released: the history takes the same folder lock.
    for (uuid, entries) in swift.history {
        let Some(id) = ids.get(&uuid) else { continue };
        match history.merge(id, entries) {
            Ok(added) => report.history += added,
            Err(e) => report.errors.push(format!("SQL history: {e}")),
        }
    }
    let patch = settings_patch(&swift.settings, &settings.get());
    if !patch.is_empty() {
        match settings.update(&serde_json::Value::Object(patch.clone())) {
            Ok(next) => {
                report.settings += patch.len();
                crate::tray::apply(app, next.keep_in_tray);
                let _ = app.emit(crate::settings::SETTINGS_EVENT, &next);
            }
            Err(e) => report.errors.push(e),
        }
    }
    Ok(report)
}

#[cfg(target_os = "macos")]
#[tauri::command]
pub async fn import_swift_connections(
    app: tauri::AppHandle,
    state: tauri::State<'_, crate::commands::AppState>,
    history: tauri::State<'_, crate::history::HistoryState>,
    settings: tauri::State<'_, crate::settings::SettingsState>,
) -> DbResult<ImportReport> {
    import_everything(&app, &state, &history, &settings).await
}

/// Marker in this app's data folder: the automatic import ran (or had nothing
/// to do) and never runs again. The manual import is always available.
const MARKER_FILE: &str = "swift-import.json";

/// Whether the automatic import may run here: not with `DBLENS_STORE=memory`
/// (tests, demos) unless `DBLENS_SWIFT_AUTO=1` asks for it.
fn auto_import_enabled() -> bool {
    let memory = std::env::var("DBLENS_STORE").as_deref() == Ok("memory");
    !memory || std::env::var("DBLENS_SWIFT_AUTO").as_deref() == Ok("1")
}

/// At the first start after moving over from dbLens for Mac: imports once,
/// without asking. None when it doesn't apply (already done, no Swift data,
/// disabled, or another window started it). The marker is written whenever
/// the import ran or the Swift app left nothing to import; after an error it
/// stays unset (the next start tries again) and the error is returned once.
#[cfg(target_os = "macos")]
#[tauri::command]
pub async fn auto_import_swift(
    app: tauri::AppHandle,
    state: tauri::State<'_, crate::commands::AppState>,
    history: tauri::State<'_, crate::history::HistoryState>,
    settings: tauri::State<'_, crate::settings::SettingsState>,
) -> DbResult<Option<ImportReport>> {
    use std::sync::atomic::{AtomicBool, Ordering};
    static STARTED: AtomicBool = AtomicBool::new(false);
    if !auto_import_enabled() {
        return Ok(None);
    }
    let marker = Store::default_dir()?.join(MARKER_FILE);
    if marker.exists() || STARTED.swap(true, Ordering::SeqCst) {
        return Ok(None);
    }
    let write_marker = |report: &ImportReport| {
        if let Some(dir) = marker.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(
            &marker,
            serde_json::json!({ "imported": report.imported, "history": report.history })
                .to_string(),
        );
    };
    let present = swift_app_dir().is_ok_and(|d| d.join(STORE_FILE).is_file());
    if !present {
        write_marker(&ImportReport::default());
        return Ok(None);
    }
    let report = import_everything(&app, &state, &history, &settings).await?;
    write_marker(&report);
    Ok(Some(report))
}

#[cfg(not(target_os = "macos"))]
#[tauri::command]
pub async fn import_swift_connections() -> DbResult<ImportReport> {
    Err(DbError::msg(
        "Importing connections from dbLens for Mac works only on macOS.",
    ))
}

#[cfg(not(target_os = "macos"))]
#[tauri::command]
pub async fn auto_import_swift() -> DbResult<Option<ImportReport>> {
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes_gcm::aead::OsRng;
    use aes_gcm::AeadCore;

    /// The columns Core Data made for `SavedConnection` (plus its bookkeeping).
    const SCHEMA: &str = "
        CREATE TABLE Z_PRIMARYKEY (Z_ENT INTEGER PRIMARY KEY, Z_NAME VARCHAR, Z_SUPER INTEGER, Z_MAX INTEGER);
        INSERT INTO Z_PRIMARYKEY VALUES (1, 'SavedConnection', 0, 3);
        CREATE TABLE Z_METADATA (Z_VERSION INTEGER PRIMARY KEY, Z_UUID VARCHAR(255), Z_PLIST BLOB);
        CREATE TABLE ZSAVEDCONNECTION (
            Z_PK INTEGER PRIMARY KEY, Z_ENT INTEGER, Z_OPT INTEGER,
            ZAGENTSCANWRITE INTEGER, ZAGENTSHIDDEN INTEGER, ZSORTINDEX INTEGER, ZSSHPORT INTEGER,
            ZCREATEDAT TIMESTAMP, ZLASTUSED TIMESTAMP,
            ZAGENTSBLOCKEDTOOLS VARCHAR, ZAGENTSNOTES VARCHAR,
            ZENGINE VARCHAR, ZFOLDERID VARCHAR, ZNAME VARCHAR,
            ZSSHHOST VARCHAR, ZSSHKEY VARCHAR, ZSSHUSER VARCHAR, ZUUID BLOB);
    ";

    const SHOP: &str = "6F9619FF-8B86-D011-B42D-00C04FC964FF";
    const CACHE: &str = "0E984725-C51C-4BF4-9960-E1C80E27ABA0";
    const LOCAL: &str = "A0B1C2D3-E4F5-4677-8899-AABBCCDDEEFF";
    const WORK: &str = "11111111-2222-4333-8444-555555555555";

    fn uuid_blob(text: &str) -> Vec<u8> {
        uuid::Uuid::parse_str(text).unwrap().as_bytes().to_vec()
    }

    /// A store like the Swift app's, in `dir`.
    fn fixture_store(dir: &Path) -> PathBuf {
        let path = dir.join(STORE_FILE);
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch(SCHEMA).unwrap();
        db.execute_batch("PRAGMA journal_mode=WAL").unwrap();
        let insert = "INSERT INTO ZSAVEDCONNECTION
            (Z_PK, Z_ENT, Z_OPT, ZAGENTSCANWRITE, ZSORTINDEX, ZSSHPORT, ZCREATEDAT, ZLASTUSED,
             ZENGINE, ZFOLDERID, ZNAME, ZSSHHOST, ZSSHKEY, ZSSHUSER, ZUUID)
            VALUES (?1, 1, 1, 0, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)";
        // 2024-01-01T00:00:00Z is 725760000 s after 2001-01-01.
        db.execute(
            insert,
            rusqlite::params![
                1,
                1,
                2222,
                725_760_000.5,
                725_846_400.0,
                "postgres",
                WORK,
                "shop",
                "bastion.example.com",
                "~/.ssh/id_ed25519",
                "ana",
                uuid_blob(SHOP)
            ],
        )
        .unwrap();
        db.execute(
            insert,
            rusqlite::params![
                2,
                0,
                0,
                725_846_400.0,
                None::<f64>,
                "redis",
                WORK.to_lowercase(),
                "cache",
                "",
                "",
                "",
                uuid_blob(CACHE)
            ],
        )
        .unwrap();
        db.execute(
            insert,
            rusqlite::params![
                3,
                0,
                0,
                725_846_400.0,
                None::<f64>,
                "sqlite",
                "",
                "local",
                "",
                "",
                "",
                uuid_blob(LOCAL)
            ],
        )
        .unwrap();
        db.execute(
            "UPDATE ZSAVEDCONNECTION SET ZAGENTSCANWRITE = 1, ZAGENTSHIDDEN = 0,
             ZAGENTSBLOCKEDTOOLS = 'execute, install_extension', ZAGENTSNOTES = 'Orders and items.'
             WHERE Z_PK = 1",
            [],
        )
        .unwrap();
        // Left open, so the rows are still in the -wal beside it.
        std::mem::forget(db);
        path
    }

    fn seal(key: &[u8; 32], plain: &[u8]) -> Vec<u8> {
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let mut out = nonce.to_vec();
        out.extend(
            Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key))
                .encrypt(&nonce, plain)
                .unwrap(),
        );
        out
    }

    fn urls_json() -> String {
        serde_json::json!({
            SHOP: "postgres://ana:secret@db.example.com:5432/shop",
            CACHE.to_lowercase(): "redis://127.0.0.1:6379/2",
            LOCAL: "sqlite:///tmp/local.db",
        })
        .to_string()
    }

    fn write_software_key(dir: &Path, key: &[u8; 32]) {
        let b64 = base64::engine::general_purpose::STANDARD.encode(key);
        std::fs::write(
            dir.join(KEY_FILE),
            format!(r#"{{"kind":"software","softwareKey":"{b64}"}}"#),
        )
        .unwrap();
    }

    fn preferences() -> Vec<u8> {
        let folders = serde_json::json!([{ "id": WORK, "name": "Work", "expanded": false }]);
        let mut dict = plist::Dictionary::new();
        dict.insert(
            "connectionFolders".into(),
            plist::Value::Data(folders.to_string().into_bytes()),
        );
        dict.insert("grid.pageSize".into(), plist::Value::Integer(200.into()));
        // Codable dates: seconds since 2001; newest first, one run by an agent.
        let history = serde_json::json!([
            { "id": "X1", "sql": "select 2", "date": 725_846_400.4, "failed": true, "viaAgent": true },
            { "id": "X2", "sql": "select 1", "date": 725_760_000.0, "failed": false },
            { "id": "X3", "sql": "  ", "date": 725_760_000.0, "failed": false },
        ]);
        dict.insert(
            format!("sqlHistory.{}", SHOP.to_lowercase()),
            plist::Value::Data(history.to_string().into_bytes()),
        );
        dict.insert("appearance.accent".into(), "mint".into());
        dict.insert("keepInMenuBar".into(), false.into());
        dict.insert("sqlPanel.shown".into(), false.into());
        dict.insert("confirmDelete".into(), true.into());
        dict.insert("agentsManageConnections".into(), true.into());
        let mut out = Vec::new();
        plist::Value::Dictionary(dict)
            .to_writer_binary(&mut out)
            .unwrap();
        out
    }

    #[test]
    fn reads_core_data_rows_from_a_copy() {
        let dir = tempfile::tempdir().unwrap();
        let path = fixture_store(dir.path());
        let before = std::fs::read(&path).unwrap();
        let wal = std::fs::read(dir.path().join("dbLens.store-wal")).unwrap();
        let mut all = read_connections(&path).unwrap();
        all.sort_by_key(|c| c.name.clone());
        assert_eq!(all.len(), 3);
        let shop = all.iter().find(|c| c.name == "shop").unwrap();
        assert_eq!(shop.uuid, SHOP);
        assert_eq!(shop.created_at.as_deref(), Some("2024-01-01T00:00:00Z"));
        assert_eq!(shop.last_used.as_deref(), Some("2024-01-02T00:00:00Z"));
        assert_eq!(shop.sort_index, 1);
        assert_eq!(shop.folder_id, WORK);
        assert_eq!(
            shop.ssh,
            Some(SshConfig {
                host: "bastion.example.com".into(),
                port: Some(2222),
                user: "ana".into(),
                key_path: "~/.ssh/id_ed25519".into(),
            })
        );
        let cache = all.iter().find(|c| c.name == "cache").unwrap();
        assert_eq!(cache.ssh, None);
        assert_eq!(cache.last_used, None);
        assert_eq!(cache.folder_id, WORK, "folder ids compare in upper case");
        // The original and its WAL are untouched.
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(
            std::fs::read(dir.path().join("dbLens.store-wal")).unwrap(),
            wal
        );
    }

    #[test]
    fn older_stores_without_newer_columns_still_read() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE ZSAVEDCONNECTION (Z_PK INTEGER PRIMARY KEY, ZNAME VARCHAR, ZUUID BLOB);",
        )
        .unwrap();
        db.execute(
            "INSERT INTO ZSAVEDCONNECTION VALUES (1, 'old', ?1)",
            [uuid_blob(SHOP)],
        )
        .unwrap();
        db.execute("INSERT INTO ZSAVEDCONNECTION VALUES (2, 'no id', NULL)", [])
            .unwrap();
        let all = read_connections_from(&db).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "old");
        assert_eq!(all[0].folder_id, "");
        assert_eq!(all[0].ssh, None);
        assert_eq!(all[0].created_at, None);
    }

    #[test]
    fn a_missing_store_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_connections(&dir.path().join(STORE_FILE)).is_err());
    }

    #[test]
    fn opens_software_sealed_urls() {
        let dir = tempfile::tempdir().unwrap();
        let key = [7u8; 32];
        write_software_key(dir.path(), &key);
        std::fs::write(
            dir.path().join(SEALED_FILE),
            seal(&key, urls_json().as_bytes()),
        )
        .unwrap();
        let urls = read_urls(dir.path()).unwrap();
        assert_eq!(urls.len(), 3);
        assert_eq!(
            urls.get(CACHE).map(String::as_str),
            Some("redis://127.0.0.1:6379/2"),
            "keys are upper case"
        );
    }

    #[test]
    fn a_fixed_vector_opens() {
        // Key 00 01 … 1f, nonce 00 … 0b, plaintext {"A":"b"}, sealed in
        // CryptoKit's combined layout (nonce, ciphertext, tag) by an
        // independent AES-GCM (Python's cryptography), not by this crate.
        let key: Vec<u8> = (0u8..32).collect();
        let hex = "000102030405060708090a0b3c209739ffc7a039f0b8b6069d573dba3c710871f48bf3f374";
        let sealed: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect();
        assert_eq!(sealed.len(), 12 + 9 + 16);
        assert_eq!(open_software(&key, &sealed).unwrap(), b"{\"A\":\"b\"}");
        let mut tampered = sealed.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert!(open_software(&key, &tampered).is_err());
        assert!(open_software(&key[..31], &sealed).is_err());
        assert!(open_software(&key, &sealed[..20]).is_err());
    }

    #[test]
    fn a_wrong_or_missing_key_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(SEALED_FILE), seal(&[1u8; 32], b"{}")).unwrap();
        assert!(read_urls(dir.path()).is_err(), "no key file");
        write_software_key(dir.path(), &[2u8; 32]);
        assert!(read_urls(dir.path()).is_err(), "another key");
        std::fs::write(dir.path().join(KEY_FILE), "not json").unwrap();
        assert!(read_urls(dir.path()).is_err());
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn secure_enclave_keys_need_a_mac() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(SEALED_FILE), seal(&[1u8; 32], b"{}")).unwrap();
        std::fs::write(
            dir.path().join(KEY_FILE),
            r#"{"kind":"secure-enclave","enclaveKey":"AAAA","peerPublicKey":"AAAA"}"#,
        )
        .unwrap();
        let error = read_urls(dir.path()).unwrap_err().to_string();
        assert!(error.contains("Secure Enclave"), "{error}");
    }

    /// Links and calls swift/SecureEnclaveOpen.swift. A made-up key never
    /// opens: no Secure Enclave on CI's virtual Macs, or one that refuses it.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_secure_enclave_helper_links_and_refuses_a_bogus_key() {
        let error = open_with_enclave(&[1u8; 64], &[4u8; 64], &[0u8; 40])
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("Secure Enclave") || error.contains("doesn't open"),
            "{error}"
        );
        assert!(open_with_enclave(&[1u8; 64], &[4u8; 64], &[0u8; 10]).is_err());
    }

    #[test]
    fn plain_urls_of_older_versions_and_none_at_all() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_urls(dir.path()).unwrap().is_empty());
        std::fs::write(dir.path().join(PLAIN_FILE), urls_json()).unwrap();
        assert_eq!(read_urls(dir.path()).unwrap().len(), 3);
    }

    #[test]
    fn subfolders_import_with_their_parents() {
        let outer = "AAAAAAAA-0000-0000-0000-000000000001";
        let inner = "AAAAAAAA-0000-0000-0000-000000000002";
        let json = serde_json::json!([
            { "id": inner.to_lowercase(), "name": "Staging", "parentID": outer.to_lowercase() },
            { "id": outer, "name": "Servers" },
        ]);
        let mut dict = plist::Dictionary::new();
        dict.insert(
            "connectionFolders".into(),
            plist::Value::Data(json.to_string().into_bytes()),
        );
        let mut bytes = Vec::new();
        plist::Value::Dictionary(dict)
            .to_writer_binary(&mut bytes)
            .unwrap();
        let folders = parse_folders(&bytes);
        assert_eq!(folders[0].parent_id.as_deref(), Some(outer));
        let uuid = "BBBBBBBB-0000-0000-0000-000000000001".to_string();
        let app = SwiftApp {
            connections: vec![SwiftConnection {
                uuid: uuid.clone(),
                name: "staging".into(),
                created_at: None,
                last_used: None,
                ssh: None,
                sort_index: 0,
                folder_id: inner.into(),
                ..Default::default()
            }],
            urls: [(uuid, "sqlite:///tmp/staging.db".to_string())].into(),
            folders,
            ..Default::default()
        };
        let mut store = Store::in_memory();
        let report = apply(&mut store, app);
        assert_eq!(report.imported, 1, "{:?}", report.errors);
        let snapshot = store.snapshot();
        let servers = snapshot
            .folders
            .iter()
            .find(|f| f.name == "Servers")
            .unwrap();
        let staging = snapshot
            .folders
            .iter()
            .find(|f| f.name == "Staging")
            .unwrap();
        assert_eq!(servers.parent_id, "");
        assert_eq!(staging.parent_id, servers.id);
        assert_eq!(snapshot.connections[0].folder_id, staging.id);
    }

    #[test]
    fn folders_come_from_the_preferences() {
        let folders = parse_folders(&preferences());
        assert_eq!(
            folders,
            vec![SwiftFolder {
                id: WORK.into(),
                name: "Work".into(),
                expanded: false,
                parent_id: None,
            }]
        );
        assert!(parse_folders(b"not a plist").is_empty());
        let mut empty = Vec::new();
        plist::Value::Dictionary(plist::Dictionary::new())
            .to_writer_xml(&mut empty)
            .unwrap();
        assert!(parse_folders(&empty).is_empty());
    }

    #[test]
    fn imports_once_with_folders_ssh_and_dates() {
        let dir = tempfile::tempdir().unwrap();
        fixture_store(dir.path());
        let key = [9u8; 32];
        write_software_key(dir.path(), &key);
        let mut urls: serde_json::Value = serde_json::from_str(&urls_json()).unwrap();
        urls.as_object_mut().unwrap().remove(LOCAL); // one without a URL
        std::fs::write(
            dir.path().join(SEALED_FILE),
            seal(&key, urls.to_string().as_bytes()),
        )
        .unwrap();
        let prefs = preferences();

        let mut store = Store::in_memory();
        let report = apply(
            &mut store,
            read_swift_app(dir.path(), Some(&prefs)).unwrap(),
        );
        assert_eq!(report.imported, 2, "{report:?}");
        assert_eq!(report.skipped, 0);
        assert_eq!(report.errors.len(), 1);
        assert!(
            report.errors[0].starts_with("local:"),
            "{:?}",
            report.errors
        );
        assert!(
            !report.errors[0].contains("sqlite://"),
            "errors never show URLs"
        );

        let snapshot = store.snapshot();
        assert_eq!(snapshot.folders.len(), 1);
        assert_eq!(snapshot.folders[0].name, "Work");
        assert!(!snapshot.folders[0].expanded);
        let work = snapshot.folders[0].id.clone();
        let names: Vec<_> = snapshot
            .connections
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(names, ["cache", "shop"], "Swift's order within the folder");
        let shop = snapshot
            .connections
            .iter()
            .find(|c| c.name == "shop")
            .unwrap();
        assert_eq!(shop.folder_id, work);
        assert_eq!(shop.created_at, "2024-01-01T00:00:00Z");
        assert_eq!(shop.last_used.as_deref(), Some("2024-01-02T00:00:00Z"));
        assert_eq!(shop.ssh.as_ref().map(|s| s.port), Some(Some(2222)));
        assert!(shop.agents_can_write);
        assert!(!shop.agents_hidden);
        assert_eq!(shop.agents_blocked_tools, ["execute", "install_extension"]);
        assert_eq!(shop.agents_notes, "Orders and items.");
        assert_eq!(report.folders, 1);
        assert!(snapshot.agents_manage_connections);
        let cache = snapshot
            .connections
            .iter()
            .find(|c| c.name == "cache")
            .unwrap();
        assert!(!cache.agents_can_write && cache.agents_notes.is_empty());
        assert_eq!(
            store.url(&shop.id),
            Some("postgres://ana:secret@db.example.com:5432/shop")
        );

        // Again: everything is already there.
        let again = apply(
            &mut store,
            read_swift_app(dir.path(), Some(&prefs)).unwrap(),
        );
        assert_eq!((again.imported, again.skipped), (0, 2), "{again:?}");
        assert_eq!(store.snapshot().connections.len(), 2);
        assert_eq!(store.snapshot().folders.len(), 1);
    }

    #[test]
    fn history_comes_over_in_order_and_merges_without_duplicates() {
        let history = parse_history(&preferences());
        let shop = &history[SHOP];
        assert_eq!(shop.len(), 2, "an empty statement is dropped");
        assert_eq!(shop[0].sql, "select 2");
        assert!(shop[0].failed && shop[0].via_agent);
        assert_eq!(shop[0].date, "2024-01-02T00:00:00Z");
        assert_eq!(shop[1].sql, "select 1");
        assert!(!shop[1].failed && !shop[1].via_agent);
        assert_eq!(shop[1].date, "2024-01-01T00:00:00Z");

        let state = crate::history::HistoryState::at(None);
        let own = HistoryEntry {
            sql: "select 3".into(),
            failed: false,
            date: "2024-06-01T00:00:00Z".into(),
            via_agent: false,
        };
        state.add("local-id", own.clone()).unwrap();
        assert_eq!(state.merge("local-id", shop.clone()).unwrap(), 2);
        assert_eq!(state.merge("local-id", shop.clone()).unwrap(), 0, "twice");
        let sqls: Vec<_> = state.list("local-id").into_iter().map(|e| e.sql).collect();
        assert_eq!(sqls, ["select 3", "select 2", "select 1"], "newest first");
    }

    #[test]
    fn history_in_a_file_merges_under_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::history::HistoryState::at(Some(dir.path().join("sql-history.json")));
        let shop = parse_history(&preferences()).remove(SHOP).unwrap();
        assert_eq!(state.merge("a", shop.clone()).unwrap(), 2);
        assert_eq!(state.merge("a", shop).unwrap(), 0);
        assert_eq!(state.list("a").len(), 2);
    }

    #[test]
    fn app_settings_only_replace_defaults() {
        let swift = parse_settings(&preferences());
        assert_eq!(swift.accent.as_deref(), Some("mint"));
        assert_eq!(swift.keep_in_menu_bar, Some(false));
        assert_eq!(swift.show_sql_panel, Some(false));
        assert_eq!(swift.confirm_delete, Some(true));
        assert_eq!(swift.agents_manage_connections, Some(true));
        let defaults = crate::settings::Settings::default();
        let patch = settings_patch(&swift, &defaults);
        assert_eq!(patch["accent"], "mint");
        assert_eq!(patch["keepInTray"], false);
        assert_eq!(patch["showSqlPanel"], false);
        assert!(!patch.contains_key("confirmDelete"), "already the default");
        // The patch is one the settings accept.
        assert!(defaults
            .patched(&serde_json::Value::Object(patch.clone()))
            .is_ok());
        // Choices made here win.
        let chosen = crate::settings::Settings {
            accent: "red".into(),
            show_sql_panel: false,
            ..defaults.clone()
        };
        let patch = settings_patch(&swift, &chosen);
        assert!(!patch.contains_key("accent") && !patch.contains_key("showSqlPanel"));
        // An accent this app doesn't have is ignored.
        let unknown = SwiftSettings {
            accent: Some("chartreuse".into()),
            ..Default::default()
        };
        assert!(settings_patch(&unknown, &defaults).is_empty());
        assert_eq!(parse_settings(b"nope"), SwiftSettings::default());
    }

    #[test]
    fn history_and_agent_flags_survive_a_second_import() {
        let dir = tempfile::tempdir().unwrap();
        fixture_store(dir.path());
        let key = [9u8; 32];
        write_software_key(dir.path(), &key);
        std::fs::write(
            dir.path().join(SEALED_FILE),
            seal(&key, urls_json().as_bytes()),
        )
        .unwrap();
        let prefs = preferences();
        let mut store = Store::in_memory();
        let app = read_swift_app(dir.path(), Some(&prefs)).unwrap();
        let (first, ids) = apply_collect(&mut store, &app);
        assert_eq!((first.imported, first.folders, first.settings), (3, 1, 1));
        let again = read_swift_app(dir.path(), Some(&prefs)).unwrap();
        let (second, ids_again) = apply_collect(&mut store, &again);
        assert_eq!((second.imported, second.skipped), (0, 3));
        assert_eq!(ids, ids_again, "skipped ones map to the same connection");
        assert_eq!(ids.len(), 3);
        assert!(ids.contains_key(SHOP));
    }

    #[test]
    fn the_report_is_camel_case_json() {
        let report = ImportReport {
            imported: 1,
            skipped: 2,
            folders: 3,
            history: 4,
            settings: 5,
            errors: vec!["x".into()],
        };
        assert_eq!(
            serde_json::to_value(report).unwrap(),
            serde_json::json!({
                "imported": 1, "skipped": 2, "folders": 3, "history": 4, "settings": 5,
                "errors": ["x"]
            })
        );
    }

    #[test]
    fn the_swift_folder_is_never_our_own() {
        let dir = tempfile::tempdir().unwrap();
        assert!(check_distinct(dir.path()).is_ok());
    }

    /// The Secure Enclave path on this Mac, end to end: a throwaway Swift data
    /// folder whose `connections.key` and `connections.enc` are written by the
    /// Swift app's own SecretBox (a copy in tests/fixtures/make_enclave/, compiled
    /// with tests/fixtures/make_enclave/main.swift), then read back through
    /// `read_swift_app` and the helper in swift/SecureEnclaveOpen.swift.
    /// Skipped (with a message) on a Mac without a usable Secure Enclave.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_secure_enclave_folder_made_by_the_swift_code_imports() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let secret_box = root.join("tests/fixtures/make_enclave/SecretBox.swift");
        let main = root.join("tests/fixtures/make_enclave/main.swift");
        let work = tempfile::tempdir().unwrap();
        let tool = work.path().join("make-fixture");
        let swiftc = std::process::Command::new("xcrun")
            .args(["--sdk", "macosx", "swiftc", "-o"])
            .arg(&tool)
            .arg(&secret_box)
            .arg(&main)
            .output()
            .expect("xcrun swiftc");
        assert!(
            swiftc.status.success(),
            "{}",
            String::from_utf8_lossy(&swiftc.stderr)
        );
        let data = work.path().join("swift-data");
        std::fs::create_dir(&data).unwrap();
        fixture_store(&data);
        let plain = work.path().join("urls.json");
        std::fs::write(&plain, urls_json()).unwrap();
        let made = std::process::Command::new(&tool)
            .arg(&data)
            .arg(&plain)
            .output()
            .unwrap();
        if made.status.code() == Some(3) {
            eprintln!("skipped: no usable Secure Enclave on this machine");
            return;
        }
        assert!(
            made.status.success(),
            "{}",
            String::from_utf8_lossy(&made.stderr)
        );
        let key_text = std::fs::read_to_string(data.join(KEY_FILE)).unwrap();
        assert!(key_text.contains("secure-enclave"), "{key_text}");

        // Through the same entry point the command uses, DBLENS_SWIFT_APP_DIR
        // included (set only here, restored after).
        std::env::set_var("DBLENS_SWIFT_APP_DIR", &data);
        let dir = swift_app_dir().unwrap();
        std::env::remove_var("DBLENS_SWIFT_APP_DIR");
        assert_eq!(dir, data);
        let prefs = preferences();
        let app = read_swift_app(&dir, Some(&prefs)).unwrap();
        assert_eq!(
            app.urls.get(SHOP).map(String::as_str),
            Some("postgres://ana:secret@db.example.com:5432/shop"),
            "the Secure Enclave key opened the URLs"
        );
        let mut store = Store::in_memory();
        let report = apply(&mut store, app);
        assert_eq!(report.imported, 3, "{report:?}");
        let shop = store
            .snapshot()
            .connections
            .into_iter()
            .find(|c| c.name == "shop")
            .unwrap();
        assert_eq!(
            store.url(&shop.id),
            Some("postgres://ana:secret@db.example.com:5432/shop")
        );
    }
}
