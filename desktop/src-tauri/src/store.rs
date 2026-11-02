//! Saved connections: names, folders, SSH settings and last use in
//! `connections.json`; the URLs (with their passwords) sealed with AES-256-GCM
//! in `connections.enc`. The key lives in the OS keyring (Windows Credential
//! Manager, Linux Secret Service) or, when there is none, in a 0600
//! `connections.key` file beside them. A damaged key or sealed file is refused,
//! never replaced, so saved URLs are not silently lost.
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
        let sealed_path = dir.join(SEALED_FILE);
        let has_sealed = sealed_path.exists();
        let (key, key_in_file) = match source {
            KeySource::File(path) => (load_key_file(&path, !has_sealed)?, true),
            KeySource::Auto => load_key_auto(dir, !has_sealed)?,
        };
        let meta = match fs::read(dir.join(META_FILE)) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(io)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => StoreSnapshot::default(),
            Err(e) => return Err(io(e)),
        };
        let urls = if has_sealed {
            let bytes = fs::read(&sealed_path).map_err(io)?;
            let plain = open_sealed(&key, &bytes)?;
            serde_json::from_slice(&plain).map_err(io)?
        } else {
            BTreeMap::new()
        };
        Ok(Store {
            dir: Some(dir.to_path_buf()),
            key,
            key_in_file,
            meta,
            urls,
        })
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
        snapshot.key_in_file = self.key_in_file;
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

/// Writes through a temp file and a rename, readable by this user only.
fn write_private(path: &Path, bytes: &[u8]) -> DbResult<()> {
    let temp = path.with_extension("tmp");
    {
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp).map_err(io)?;
        file.write_all(bytes).map_err(io)?;
        file.sync_all().map_err(io)?;
    }
    fs::rename(&temp, path).map_err(io)
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
}
