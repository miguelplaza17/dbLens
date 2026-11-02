//! Saved connections: names, folders, SSH settings and last use in
//! `connections.json`; the URLs (with their passwords) sealed with AES-256-GCM
//! in `connections.enc`. The key lives in the OS keyring (Windows Credential
//! Manager, Linux Secret Service) or, when there is none, in a 0600
//! `connections.key` file beside them. On macOS the key file is the default
//! (see `KEY_FILE_FIRST`). A damaged key or sealed file is refused,
//! never replaced, so saved URLs are not silently lost.
//!
//! The app and `dblens --mcp` (mcp.rs) are separate processes on the same
//! folder: every read or change goes through `Store::transact`, which holds an
//! advisory lock on `connections.lock` and reloads both files first, so one
//! never overwrites the other's changes with a stale copy, or reads a torn pair.
//!
//! Folder: `directories::ProjectDirs::from("app", "dblens", "dbLens").data_dir()`
//! (Linux `~/.local/share/dblens`, Windows `%APPDATA%\dblens\dbLens\data`),
//! or `$DBLENS_STORE_DIR` for development and tests.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use aes_gcm::aead::{Aead, KeyInit, OsRng};
use aes_gcm::{AeadCore, Aes256Gcm, Key, Nonce};
use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::engine::Engine;
use crate::session::{DbError, DbResult};
use crate::ssh::SshConfig;

const KEYRING_SERVICE: &str = "app.dblens.desktop";
const KEYRING_USER: &str = "connections-key";
const META_FILE: &str = "connections.json";
const SEALED_FILE: &str = "connections.enc";
const KEY_FILE: &str = "connections.key";
const LOCK_FILE: &str = "connections.lock";

/// A saved connection as the sidebar shows it. The URL is never in here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedConnection {
    pub id: String,
    pub name: String,
    pub engine: Engine,
    /// RFC 3339.
    pub created_at: String,
    /// RFC 3339; None until first connected.
    #[serde(default)]
    pub last_used: Option<String>,
    /// "" is no folder.
    #[serde(default)]
    pub folder_id: String,
    /// Order within its folder.
    #[serde(default)]
    pub sort_index: i64,
    #[serde(default)]
    pub ssh: Option<SshConfig>,
    /// "Let Agents Write": the MCP server (mcp.rs) may run changes here. Off by default.
    #[serde(default)]
    pub agents_can_write: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionFolder {
    pub id: String,
    pub name: String,
    #[serde(default = "yes")]
    pub expanded: bool,
}

fn yes() -> bool {
    true
}

/// Everything the sidebar lists.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreSnapshot {
    pub connections: Vec<SavedConnection>,
    pub folders: Vec<ConnectionFolder>,
    /// "Let agents add and remove connections" (MCP, mcp.rs). Off by default.
    #[serde(default)]
    pub agents_manage_connections: bool,
    /// True when the sealing key sits in `connections.key` beside the sealed
    /// URLs (no keyring on this system), so the frontend can say so. Never saved.
    #[serde(
        default,
        skip_deserializing,
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub key_in_file: bool,
}

/// What the connection sheet sends to save. With `id`, it edits that
/// connection, and a `url` still showing the mask (•••) keeps the saved password.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionDraft {
    #[serde(default)]
    pub id: Option<String>,
    /// Empty uses the URL's suggested name.
    #[serde(default)]
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub ssh: Option<SshConfig>,
    #[serde(default)]
    pub folder_id: Option<String>,
}

/// Where the sealing key comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeySource {
    /// The OS keyring, else a key file in the store folder.
    Auto,
    /// Only this key file (tests, or systems where the keyring misbehaves).
    File(PathBuf),
}

pub struct Store {
    dir: Option<PathBuf>,
    key: Key<Aes256Gcm>,
    /// The key came from (or went to) the key file, not the keyring.
    key_in_file: bool,
    meta: StoreSnapshot,
    urls: BTreeMap<String, String>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store")
            .field("dir", &self.dir)
            .field("meta", &self.meta)
            .finish_non_exhaustive()
    }
}

fn io(error: impl std::fmt::Display) -> DbError {
    DbError::Message(format!("Saved connections: {error}"))
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

impl Store {
    /// The default folder for this OS, or `$DBLENS_STORE_DIR`.
    pub fn default_dir() -> DbResult<PathBuf> {
        if let Some(dir) = std::env::var_os("DBLENS_STORE_DIR") {
            return Ok(PathBuf::from(dir));
        }
        directories::ProjectDirs::from("app", "dblens", "dbLens")
            .map(|d| d.data_dir().to_path_buf())
            .ok_or_else(|| io("no home folder"))
    }

    /// A store that keeps everything in memory (tests, demo mode).
    pub fn in_memory() -> Store {
        Store {
            dir: None,
            key: Aes256Gcm::generate_key(OsRng),
            key_in_file: false,
            meta: StoreSnapshot::default(),
            urls: BTreeMap::new(),
        }
    }

    /// Opens (or creates) the store in `dir`. Blocking: call it off the async
    /// runtime (spawn_blocking), since the keyring may talk to D-Bus.
    pub fn open(dir: &Path, source: KeySource) -> DbResult<Store> {
        create_private_dir(dir)?;
        let _lock = lock_dir(dir)?;
        let has_sealed = dir.join(SEALED_FILE).exists();
        let (key, key_in_file) = match source {
            KeySource::File(path) => (load_key_file(&path, !has_sealed)?, true),
            KeySource::Auto => load_key_auto(dir, !has_sealed)?,
        };
        let (meta, urls) = read_files(dir, &key)?;
        Ok(Store {
            dir: Some(dir.to_path_buf()),
            key,
            key_in_file,
            meta,
            urls,
        })
    }

    /// Runs `body` with the folder locked against the other process (the app
    /// or `dblens --mcp`) after reloading what that process may have saved, so
    /// `body`'s changes are made to, and persisted from, the current files.
    /// Blocking (file lock and reads). A store in memory just runs `body`.
    pub fn transact<T>(&mut self, body: impl FnOnce(&mut Store) -> DbResult<T>) -> DbResult<T> {
        let Some(dir) = self.dir.clone() else {
            return body(self);
        };
        let _lock = lock_dir(&dir)?;
        let (meta, urls) = read_files(&dir, &self.key)?;
        self.meta = meta;
        self.urls = urls;
        body(self)
    }

    /// Connections ordered by folder, then position; folders as saved.
    pub fn snapshot(&self) -> StoreSnapshot {
        let mut snapshot = self.meta.clone();
        snapshot.connections.sort_by(|a, b| {
            a.folder_id
                .cmp(&b.folder_id)
                .then(a.sort_index.cmp(&b.sort_index))
                .then(a.name.cmp(&b.name))
        });
        // On macOS the file is the design, not a fallback worth a notice.
        snapshot.key_in_file = self.key_in_file && !KEY_FILE_FIRST;
        snapshot
    }

    pub fn get(&self, id: &str) -> Option<&SavedConnection> {
        self.meta.connections.iter().find(|c| c.id == id)
    }

    /// The saved URL, with its password. Never send it to the frontend as is.
    pub fn url(&self, id: &str) -> Option<&str> {
        self.urls.get(id).map(String::as_str)
    }

    /// The saved URL with passwords masked, for the edit sheet.
    pub fn masked_url(&self, id: &str) -> Option<String> {
        self.url(id).map(crate::url::redact)
    }

    pub fn save(&mut self, draft: ConnectionDraft) -> DbResult<SavedConnection> {
        let mut url = crate::url::clean(&draft.url);
        if let Some(saved) = draft.id.as_deref().and_then(|id| self.urls.get(id)) {
            url = crate::url::unmask(&url, saved);
        }
        let summary = crate::connector::summary(&url)
            .ok_or_else(|| DbError::msg("This doesn't look like a database URL."))?;
        let name = if draft.name.trim().is_empty() {
            summary.suggested_name.clone()
        } else {
            draft.name.trim().to_string()
        };
        let ssh = draft.ssh.filter(|s| !s.host.trim().is_empty());
        let existing = draft
            .id
            .as_deref()
            .and_then(|id| self.meta.connections.iter().position(|c| c.id == id));
        let saved = match existing {
            Some(at) => {
                let c = &mut self.meta.connections[at];
                c.name = name;
                c.engine = summary.engine;
                c.ssh = ssh;
                if let Some(folder) = draft.folder_id {
                    c.folder_id = folder;
                }
                c.clone()
            }
            None => {
                let folder_id = draft.folder_id.unwrap_or_default();
                let sort_index = self
                    .meta
                    .connections
                    .iter()
                    .filter(|c| c.folder_id == folder_id)
                    .map(|c| c.sort_index + 1)
                    .max()
                    .unwrap_or(0);
                let c = SavedConnection {
                    id: uuid::Uuid::new_v4().to_string(),
                    name,
                    engine: summary.engine,
                    created_at: now(),
                    last_used: None,
                    folder_id,
                    sort_index,
                    ssh,
                    agents_can_write: false,
                };
                self.meta.connections.push(c.clone());
                c
            }
        };
        self.urls.insert(saved.id.clone(), url);
        self.persist()?;
        Ok(saved)
    }

    pub fn delete(&mut self, id: &str) -> DbResult<()> {
        self.meta.connections.retain(|c| c.id != id);
        self.urls.remove(id);
        self.persist()
    }

    pub fn rename(&mut self, id: &str, name: &str) -> DbResult<SavedConnection> {
        let c = self
            .meta
            .connections
            .iter_mut()
            .find(|c| c.id == id)
            .ok_or_else(|| DbError::msg("No such connection."))?;
        if !name.trim().is_empty() {
            c.name = name.trim().to_string();
        }
        let c = c.clone();
        self.persist()?;
        Ok(c)
    }

    /// "Let Agents Write" for one connection.
    pub fn set_agents_can_write(&mut self, id: &str, allowed: bool) -> DbResult<SavedConnection> {
        let c = self
            .meta
            .connections
            .iter_mut()
            .find(|c| c.id == id)
            .ok_or_else(|| DbError::msg("No such connection."))?;
        c.agents_can_write = allowed;
        let c = c.clone();
        self.persist()?;
        Ok(c)
    }

    /// "Let agents add and remove connections", for all of them.
    pub fn set_agents_manage_connections(&mut self, allowed: bool) -> DbResult<()> {
        self.meta.agents_manage_connections = allowed;
        self.persist()
    }

    /// Keeps the dates a connection brought from another app (the dbLens for
    /// Mac importer); None leaves that date as it is.
    pub fn set_dates(
        &mut self,
        id: &str,
        created_at: Option<String>,
        last_used: Option<String>,
    ) -> DbResult<()> {
        if let Some(c) = self.meta.connections.iter_mut().find(|c| c.id == id) {
            if let Some(created_at) = created_at {
                c.created_at = created_at;
            }
            if last_used.is_some() {
                c.last_used = last_used;
            }
            self.persist()?;
        }
        Ok(())
    }

    /// Marks a connection as just used.
    pub fn touch(&mut self, id: &str) -> DbResult<()> {
        if let Some(c) = self.meta.connections.iter_mut().find(|c| c.id == id) {
            c.last_used = Some(now());
            self.persist()?;
        }
        Ok(())
    }

    pub fn add_folder(&mut self, name: &str) -> DbResult<ConnectionFolder> {
        let folder = ConnectionFolder {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.trim().to_string(),
            expanded: true,
        };
        self.meta.folders.push(folder.clone());
        self.persist()?;
        Ok(folder)
    }

    pub fn rename_folder(&mut self, id: &str, name: &str) -> DbResult<()> {
        if let Some(f) = self.meta.folders.iter_mut().find(|f| f.id == id) {
            f.name = name.trim().to_string();
        }
        self.persist()
    }

    pub fn set_folder_expanded(&mut self, id: &str, expanded: bool) -> DbResult<()> {
        if let Some(f) = self.meta.folders.iter_mut().find(|f| f.id == id) {
            f.expanded = expanded;
        }
        self.persist()
    }

    /// Removes a folder; its connections move to the top level.
    pub fn delete_folder(&mut self, id: &str) -> DbResult<()> {
        self.meta.folders.retain(|f| f.id != id);
        for c in self
            .meta
            .connections
            .iter_mut()
            .filter(|c| c.folder_id == id)
        {
            c.folder_id.clear();
        }
        self.persist()
    }

    /// Moves a connection into `folder_id` ("" for none), before `before_id`
    /// (None: at the end), and renumbers that folder.
    pub fn move_connection(
        &mut self,
        id: &str,
        folder_id: &str,
        before_id: Option<&str>,
    ) -> DbResult<()> {
        let Some(mut moving) = self.get(id).cloned() else {
            return Err(DbError::msg("No such connection."));
        };
        moving.folder_id = folder_id.to_string();
        let mut level: Vec<SavedConnection> = self
            .snapshot()
            .connections
            .into_iter()
            .filter(|c| c.folder_id == folder_id && c.id != id)
            .collect();
        let at = before_id
            .and_then(|b| level.iter().position(|c| c.id == b))
            .unwrap_or(level.len());
        level.insert(at, moving);
        for (position, item) in level.iter().enumerate() {
            if let Some(c) = self.meta.connections.iter_mut().find(|c| c.id == item.id) {
                c.folder_id = folder_id.to_string();
                c.sort_index = position as i64;
            }
        }
        self.persist()
    }

    fn persist(&self) -> DbResult<()> {
        let Some(dir) = &self.dir else { return Ok(()) };
        let plain = serde_json::to_vec(&self.urls).map_err(io)?;
        write_private(&dir.join(SEALED_FILE), &seal(&self.key, &plain)?)?;
        write_private(
            &dir.join(META_FILE),
            &serde_json::to_vec_pretty(&self.meta).map_err(io)?,
        )
    }
}

/// The folder's advisory lock (`flock`, `LockFileEx`), held until dropped.
/// Every process takes it around reading and writing `connections.json` and
/// `connections.enc`. The OS also drops it when the process dies.
struct DirLock(fs::File);

impl Drop for DirLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            // SAFETY: the descriptor is open for as long as `self.0` lives.
            unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            // SAFETY: the handle is open for as long as `self.0` lives.
            unsafe {
                windows_sys::Win32::Storage::FileSystem::UnlockFile(
                    self.0.as_raw_handle() as _,
                    0,
                    0,
                    u32::MAX,
                    u32::MAX,
                )
            };
        }
    }
}

fn lock_dir(dir: &Path) -> DbResult<DirLock> {
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(dir.join(LOCK_FILE)).map_err(io)?;
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        loop {
            // SAFETY: a valid open descriptor; blocks until the lock is free.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
                break;
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(io(error));
            }
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{LockFileEx, LOCKFILE_EXCLUSIVE_LOCK};
        let mut overlapped: windows_sys::Win32::System::IO::OVERLAPPED =
            // SAFETY: OVERLAPPED is plain data; all zeros locks from offset 0.
            unsafe { std::mem::zeroed() };
        // SAFETY: a valid open handle; blocks until the lock is free.
        let locked = unsafe {
            LockFileEx(
                file.as_raw_handle() as _,
                LOCKFILE_EXCLUSIVE_LOCK,
                0,
                u32::MAX,
                u32::MAX,
                &mut overlapped,
            )
        };
        if locked == 0 {
            return Err(io(std::io::Error::last_os_error()));
        }
    }
    Ok(DirLock(file))
}

/// The names and the sealed URLs as saved now (empty when not saved yet).
fn read_files(
    dir: &Path,
    key: &Key<Aes256Gcm>,
) -> DbResult<(StoreSnapshot, BTreeMap<String, String>)> {
    let meta = match fs::read(dir.join(META_FILE)) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(io)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => StoreSnapshot::default(),
        Err(e) => return Err(io(e)),
    };
    let urls = match fs::read(dir.join(SEALED_FILE)) {
        Ok(bytes) => serde_json::from_slice(&open_sealed(key, &bytes)?).map_err(io)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
        Err(e) => return Err(io(e)),
    };
    Ok((meta, urls))
}

/// nonce (12 bytes) followed by the ciphertext and tag.
fn seal(key: &Key<Aes256Gcm>, plain: &[u8]) -> DbResult<Vec<u8>> {
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let mut out = nonce.to_vec();
    out.extend(
        Aes256Gcm::new(key)
            .encrypt(&nonce, plain)
            .map_err(|_| io("couldn't seal"))?,
    );
    Ok(out)
}

fn open_sealed(key: &Key<Aes256Gcm>, bytes: &[u8]) -> DbResult<Vec<u8>> {
    if bytes.len() < 12 + 16 {
        return Err(io("connections.enc is damaged"));
    }
    let (nonce, body) = bytes.split_at(12);
    Aes256Gcm::new(key)
        .decrypt(Nonce::from_slice(nonce), body)
        .map_err(|_| {
            io("connections.enc can't be opened with this key (another computer or user?)")
        })
}

/// Writes through a temp file and a rename, readable by this user only. The
/// temp name is unique, so two processes saving at once never share one.
fn write_private(path: &Path, bytes: &[u8]) -> DbResult<()> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("store");
    let temp = path.with_file_name(format!(".{name}.{}.tmp", uuid::Uuid::new_v4().simple()));
    let written = (|| {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, path)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written.map_err(io)
}

fn decode_key(text: &str) -> DbResult<Key<Aes256Gcm>> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(text.trim())
        .map_err(|_| io("the key is damaged"))?;
    if bytes.len() != 32 {
        return Err(io("the key is damaged"));
    }
    Ok(*Key::<Aes256Gcm>::from_slice(&bytes))
}

fn encode_key(key: &Key<Aes256Gcm>) -> String {
    base64::engine::general_purpose::STANDARD.encode(key.as_slice())
}

/// The store folder, readable by this user only (0700 on Unix; an existing
/// folder is tightened too).
fn create_private_dir(dir: &Path) -> DbResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        if !dir.exists() {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)
                .map_err(io)?;
        }
        let mode = fs::metadata(dir).map_err(io)?.permissions().mode();
        if mode & 0o077 != 0 {
            fs::set_permissions(dir, fs::Permissions::from_mode(mode & 0o700)).map_err(io)?;
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(dir).map_err(io)
    }
}

/// An existing key file that others can read is made 0600 before use.
fn tighten_key_file(path: &Path) -> DbResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(path).map_err(io)?.permissions().mode();
        if mode & 0o077 != 0 {
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(io)?;
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// The key file's key; a new one only when `create` and the file is absent.
fn load_key_file(path: &Path, create: bool) -> DbResult<Key<Aes256Gcm>> {
    match fs::read_to_string(path) {
        Ok(text) => {
            tighten_key_file(path)?;
            decode_key(&text)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && create => {
            let key = Aes256Gcm::generate_key(OsRng);
            write_private(path, encode_key(&key).as_bytes())?;
            Ok(key)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(io("the key is missing")),
        Err(e) => Err(io(e)),
    }
}

/// What a keyring error means for where the key goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyringTrouble {
    /// No keyring here (no Secret Service, WSL, a bare window manager): the
    /// key file is the fallback.
    Absent,
    /// A keyring that is locked, or whose unlock prompt was dismissed: try
    /// again later, never fall back for good.
    Locked,
}

fn keyring_trouble(error: &keyring::Error) -> KeyringTrouble {
    match error {
        // Windows reports a missing credential store (no logon session) this way.
        keyring::Error::NoStorageAccess(_) if cfg!(windows) => KeyringTrouble::Absent,
        keyring::Error::NoStorageAccess(_) => KeyringTrouble::Locked,
        _ => KeyringTrouble::Absent,
    }
}

fn keyring_locked() -> DbError {
    io("the system keyring is locked or access was denied. Unlock it, then try again.")
}

/// Calls the keyring, once more after a short pause when it looks absent, so a
/// D-Bus that is still starting is not taken for no keyring at all.
fn with_keyring<T>(call: impl Fn() -> keyring::Result<T>) -> keyring::Result<T> {
    match call() {
        Err(e)
            if keyring_trouble(&e) == KeyringTrouble::Absent
                && !matches!(e, keyring::Error::NoEntry) =>
        {
            std::thread::sleep(std::time::Duration::from_millis(300));
            call()
        }
        other => other,
    }
}

/// macOS: the Keychain ties an item to the app's code signature, so every ad
/// hoc or re-signed build (CI signs ad hoc without secrets) asks again, and a
/// dismissed prompt reads as denied: the store would not open. As the Swift
/// app did, the key lives in the 0600 key file instead.
const KEY_FILE_FIRST: bool = cfg!(target_os = "macos");

/// The key-file-first path (macOS): a new key goes straight to the file; a
/// key an earlier build left in the Keychain is read once and copied to the
/// file, so later launches never touch the Keychain.
fn load_key_keychain_once(file: &Path, create: bool) -> DbResult<(Key<Aes256Gcm>, bool)> {
    if create {
        return Ok((load_key_file(file, true)?, true));
    }
    let entry =
        keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER).map_err(|_| io("the key is missing"))?;
    match entry.get_password() {
        Ok(text) => {
            let key = decode_key(&text)?;
            write_private(file, encode_key(&key).as_bytes())?;
            Ok((key, true))
        }
        Err(keyring::Error::NoEntry) => Err(io("the key is missing")),
        Err(_) => Err(io(
            "the Keychain did not give the key that seals saved passwords. Allow access when macOS asks, then try again.",
        )),
    }
}

/// Keyring first; a key file already in the folder wins (it was the fallback
/// before); a new key goes to the keyring, or to the file only when there is
/// no keyring at all. A locked keyring is an error, so one bad moment never
/// leaves the key beside the sealed file for good. The flag is true when the
/// key is in the file.
fn load_key_auto(dir: &Path, create: bool) -> DbResult<(Key<Aes256Gcm>, bool)> {
    let file = dir.join(KEY_FILE);
    if file.exists() {
        return Ok((load_key_file(&file, false)?, true));
    }
    if KEY_FILE_FIRST {
        return load_key_keychain_once(&file, create);
    }
    let entry = match keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER) {
        Ok(entry) => entry,
        Err(e) if keyring_trouble(&e) == KeyringTrouble::Locked => return Err(keyring_locked()),
        Err(_) => return Ok((load_key_file(&file, create)?, true)),
    };
    match with_keyring(|| entry.get_password()) {
        Ok(text) => return Ok((decode_key(&text)?, false)),
        Err(keyring::Error::NoEntry) => {}
        Err(e) if keyring_trouble(&e) == KeyringTrouble::Locked => return Err(keyring_locked()),
        Err(_) => return Ok((load_key_file(&file, create)?, true)),
    }
    if !create {
        return Err(io("the key is missing from the keyring"));
    }
    let key = Aes256Gcm::generate_key(OsRng);
    match with_keyring(|| entry.set_password(&encode_key(&key))) {
        Ok(()) => Ok((key, false)),
        Err(e) if keyring_trouble(&e) == KeyringTrouble::Locked => Err(keyring_locked()),
        Err(_) => {
            write_private(&file, encode_key(&key).as_bytes())?;
            Ok((key, true))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(url: &str) -> ConnectionDraft {
        ConnectionDraft {
            id: None,
            name: String::new(),
            url: url.into(),
            ssh: None,
            folder_id: None,
        }
    }

    #[test]
    fn saves_seals_and_reopens() {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("test.key");
        let mut store = Store::open(dir.path(), KeySource::File(key.clone())).unwrap();
        let saved = store
            .save(draft("DATABASE_URL=postgres://ana:s3cret@db.x.com/loja"))
            .unwrap();
        assert_eq!(saved.name, "loja @ db.x.com");
        assert_eq!(saved.engine, Engine::Postgres);
        let sealed = fs::read(dir.path().join(SEALED_FILE)).unwrap();
        assert!(!String::from_utf8_lossy(&sealed).contains("s3cret"));
        assert!(!fs::read_to_string(dir.path().join(META_FILE))
            .unwrap()
            .contains("s3cret"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(dir.path().join(SEALED_FILE))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }

        let mut again = Store::open(dir.path(), KeySource::File(key)).unwrap();
        assert_eq!(
            again.url(&saved.id),
            Some("postgres://ana:s3cret@db.x.com/loja")
        );
        assert_eq!(
            again.masked_url(&saved.id).as_deref(),
            Some("postgres://ana:•••@db.x.com/loja")
        );
        // Editing with the mask still in place keeps the password.
        let mut edit = draft("postgres://ana:•••@db.x.com/outra");
        edit.id = Some(saved.id.clone());
        again.save(edit).unwrap();
        assert_eq!(
            again.url(&saved.id),
            Some("postgres://ana:s3cret@db.x.com/outra")
        );
    }

    #[cfg(unix)]
    #[test]
    fn keeps_the_folder_and_key_file_private() {
        use std::os::unix::fs::PermissionsExt;
        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("store");
        let key = dir.join("k");
        Store::open(&dir, KeySource::File(key.clone()))
            .unwrap()
            .save(draft("redis://localhost/0"))
            .unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        fs::set_permissions(&key, fs::Permissions::from_mode(0o644)).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        let store = Store::open(&dir, KeySource::File(key.clone())).unwrap();
        assert_eq!(mode(&key), 0o600);
        assert_eq!(mode(&dir), 0o700);
        assert!(store.snapshot().key_in_file);
        // The flag reaches the frontend but is never written to connections.json.
        assert!(!fs::read_to_string(dir.join(META_FILE))
            .unwrap()
            .contains("keyInFile"));
    }

    #[test]
    fn locked_keyrings_are_not_taken_for_missing_ones() {
        let locked = keyring::Error::NoStorageAccess("locked".into());
        let absent = keyring::Error::PlatformFailure("no dbus".into());
        assert_eq!(keyring_trouble(&absent), KeyringTrouble::Absent);
        if !cfg!(windows) {
            assert_eq!(keyring_trouble(&locked), KeyringTrouble::Locked);
        }
    }

    #[test]
    fn refuses_a_damaged_key() {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("k");
        Store::open(dir.path(), KeySource::File(key.clone()))
            .unwrap()
            .save(draft("redis://localhost/0"))
            .unwrap();
        fs::write(&key, "not a key").unwrap();
        assert!(Store::open(dir.path(), KeySource::File(key.clone())).is_err());
        assert_eq!(fs::read_to_string(&key).unwrap(), "not a key");
        let other = dir.path().join("other");
        assert!(Store::open(dir.path(), KeySource::File(other)).is_err());
    }

    #[test]
    fn folders_and_order() {
        let mut store = Store::in_memory();
        let a = store.save(draft("sqlite:///tmp/a.db")).unwrap();
        let b = store.save(draft("sqlite:///tmp/b.db")).unwrap();
        let folder = store.add_folder("Work").unwrap();
        store.move_connection(&b.id, "", Some(&a.id)).unwrap();
        let names: Vec<_> = store
            .snapshot()
            .connections
            .iter()
            .map(|c| c.name.clone())
            .collect();
        assert_eq!(names, vec!["b.db", "a.db"]);
        store.move_connection(&a.id, &folder.id, None).unwrap();
        assert_eq!(store.get(&a.id).unwrap().folder_id, folder.id);
        store.delete_folder(&folder.id).unwrap();
        assert_eq!(store.get(&a.id).unwrap().folder_id, "");
        store.rename(&a.id, "  Mine ").unwrap();
        assert_eq!(store.get(&a.id).unwrap().name, "Mine");
        store.touch(&a.id).unwrap();
        assert!(store.get(&a.id).unwrap().last_used.is_some());
        store.delete(&a.id).unwrap();
        assert!(store.url(&a.id).is_none());
        assert!(store.save(draft("hello")).is_err());
    }

    /// Two processes on one folder (the app and `dblens --mcp`): changes made
    /// through `transact` start from the other's saves and never undo them.
    #[test]
    fn two_stores_on_one_folder_keep_each_others_changes() {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("test.key");
        let mut app = Store::open(dir.path(), KeySource::File(key.clone())).unwrap();
        let kept = app
            .transact(|s| s.save(draft("postgres://a@h/kept")))
            .unwrap();
        let mut mcp = Store::open(dir.path(), KeySource::File(key)).unwrap();
        let added = mcp
            .transact(|s| s.save(draft("postgres://b:pw@h/added")))
            .unwrap();
        // The app's next change keeps what the other one added, URL included.
        app.transact(|s| s.rename(&kept.id, "Kept")).unwrap();
        assert_eq!(app.url(&added.id), Some("postgres://b:pw@h/added"));
        mcp.transact(|s| s.delete(&added.id)).unwrap();
        // ...and a removal stays removed.
        app.transact(|s| s.set_agents_can_write(&kept.id, true))
            .unwrap();
        assert!(app.get(&added.id).is_none() && app.url(&added.id).is_none());
        let names: Vec<String> = mcp
            .transact(|s| Ok(s.snapshot()))
            .unwrap()
            .connections
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(names, ["Kept"]);
        // No temp files are left behind.
        let leftovers = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(leftovers, 0);
    }
}
