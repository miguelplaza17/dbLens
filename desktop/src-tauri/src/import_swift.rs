//! File > Import Connections from dbLens for Mac…: copies the saved
//! connections of the Swift app (Sources/, discontinued) into this app's store.
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
//! - Folders are in its preferences (`connectionFolders`, JSON in a plist).
//!
//! Nothing there is written. This app's own store is a different folder
//! (`Store::default_dir`), checked before importing.

// The readers are platform independent and unit-tested everywhere; only the
// command uses them on macOS.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::Engine as _;
use rusqlite::types::ValueRef;
use serde::{Deserialize, Serialize};

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
#[derive(Debug, Clone, PartialEq, Eq)]
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
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SwiftFolder {
    pub id: String,
    pub name: String,
    #[serde(default = "yes")]
    pub expanded: bool,
}

fn yes() -> bool {
    true
}

/// Everything read from the Swift app.
#[derive(Debug, Default)]
pub struct SwiftApp {
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
            ..f
        })
        .collect()
}

/// The preferences as cfprefsd has them now (`defaults export`, which sees
/// changes not yet written to disk), else the file in ~/Library/Preferences.
#[cfg(target_os = "macos")]
fn read_preferences() -> Option<Vec<u8>> {
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
    })
}

/// Adds the Swift app's connections to `store`, in its sidebar order, with
/// their folders (an existing folder of the same name is reused), SSH
/// settings and dates. A connection with the same URL and name is skipped,
/// so importing twice adds nothing.
pub fn apply(store: &mut Store, app: SwiftApp) -> ImportReport {
    let mut report = ImportReport::default();
    let snapshot = store.snapshot();
    let mut present: HashSet<(String, String)> = snapshot
        .connections
        .iter()
        .filter_map(|c| store.url(&c.id).map(|u| (u.to_string(), c.name.clone())))
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
        if present.contains(&(url.clone(), name.clone())) {
            report.skipped += 1;
            continue;
        }
        let folder_id = match app.folders.iter().find(|f| f.id == c.folder_id) {
            None => String::new(),
            Some(folder) => match folders.get(&folder.id) {
                Some(id) => id.clone(),
                None => match local_folder(store, folder) {
                    Ok(id) => {
                        folders.insert(folder.id.clone(), id.clone());
                        id
                    }
                    Err(e) => {
                        report.errors.push(format!("{}: {e}", folder.name));
                        String::new()
                    }
                },
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
                present.insert((url, saved.name));
                report.imported += 1;
            }
            Err(e) => report.errors.push(format!("{label}: {e}")),
        }
    }
    report
}

/// The folder of that name in `store`, made (with its expanded state) when
/// there is none.
fn local_folder(store: &mut Store, folder: &SwiftFolder) -> DbResult<String> {
    let name = folder.name.trim();
    if let Some(existing) = store.snapshot().folders.iter().find(|f| f.name == name) {
        return Ok(existing.id.clone());
    }
    let made = store.add_folder(name)?;
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

#[cfg(target_os = "macos")]
#[tauri::command]
pub async fn import_swift_connections(
    state: tauri::State<'_, crate::commands::AppState>,
) -> DbResult<ImportReport> {
    let app = tokio::task::spawn_blocking(|| {
        let dir = swift_app_dir()?;
        check_distinct(&dir)?;
        read_swift_app(&dir, read_preferences().as_deref())
    })
    .await
    .map_err(|e| DbError::Message(e.to_string()))??;
    state.with_store(|store| Ok(apply(store, app))).await
}

#[cfg(not(target_os = "macos"))]
#[tauri::command]
pub async fn import_swift_connections() -> DbResult<ImportReport> {
    Err(DbError::msg(
        "Importing connections from dbLens for Mac works only on macOS.",
    ))
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
            ZAGENTSCANWRITE INTEGER, ZSORTINDEX INTEGER, ZSSHPORT INTEGER,
            ZCREATEDAT TIMESTAMP, ZLASTUSED TIMESTAMP,
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
    fn folders_come_from_the_preferences() {
        let folders = parse_folders(&preferences());
        assert_eq!(
            folders,
            vec![SwiftFolder {
                id: WORK.into(),
                name: "Work".into(),
                expanded: false
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
    fn the_report_is_camel_case_json() {
        let report = ImportReport {
            imported: 1,
            skipped: 2,
            errors: vec!["x".into()],
        };
        assert_eq!(
            serde_json::to_value(report).unwrap(),
            serde_json::json!({ "imported": 1, "skipped": 2, "errors": ["x"] })
        );
    }

    #[test]
    fn the_swift_folder_is_never_our_own() {
        let dir = tempfile::tempdir().unwrap();
        assert!(check_distinct(dir.path()).is_ok());
    }
}
