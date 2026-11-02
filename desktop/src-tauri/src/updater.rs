//! In-app updates (Sources/dbLens/Updater.swift, which used Sparkle): the
//! Help > Check for Updates row, Settings > Updates, and a background check at
//! most once a day. Releases are read from the endpoint in `plugins.updater` in
//! tauri.conf.json (GitHub Releases' `latest.json`); the plugin installs a package
//! only when its signature matches the public key there.
//!
//! Without a real public key (the placeholder in tauri.conf.json) updates are
//! off: a check says so, and nothing is fetched or installed.
//!
//! Settings used: `checkForUpdates` (the daily check) and `updateAutomatically`
//! (download when found, install when the app quits). Every window gets the
//! `update-available` event with an `UpdateInfo` when an update is found.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::close_guard;
use crate::settings;
use crate::store::Store;

/// Sent to every window when an update is found; the payload is an `UpdateInfo`.
pub const AVAILABLE_EVENT: &str = "update-available";
/// The background check runs at most this often.
const EVERY: Duration = Duration::from_secs(24 * 60 * 60);
/// The first background check waits this long after start.
const FIRST_CHECK_DELAY: Duration = Duration::from_secs(30);
/// How often the background loop looks at the clock (cheap; the check itself is daily).
const POLL: Duration = Duration::from_secs(60 * 60);
/// The file that remembers when the last check ran (in the data folder).
const STAMP_FILE: &str = "update-check.txt";
const NOT_CONFIGURED: &str =
    "Updates are not configured in this build: the public key in tauri.conf.json is a placeholder.";

/// What the Settings window and the notice need.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub available: bool,
    pub version: Option<String>,
    pub notes: Option<String>,
}

/// An update found by the last check, with its package once it is downloaded.
struct Pending {
    update: Update,
    /// Downloaded and verified ahead of time (automatic updates).
    bytes: Option<Vec<u8>>,
}

#[derive(Default)]
pub struct UpdaterState {
    pending: tokio::sync::Mutex<Option<Pending>>,
    /// Seconds since the epoch of the last check that ran (None: never, this run).
    last_check: Mutex<Option<u64>>,
    /// Set by install_update: relaunch the app once it has quit.
    restart_after_exit: AtomicBool,
}

/// Whether `text` is a minisign public key as Tauri writes it: base64 of an
/// "untrusted comment: ..." text. The placeholder is not.
pub fn is_real_pubkey(text: &str) -> bool {
    base64::engine::general_purpose::STANDARD
        .decode(text.trim())
        .is_ok_and(|bytes| bytes.starts_with(b"untrusted comment:"))
}

/// Ok when this build has a real public key; otherwise why updates are off.
fn configured(app: &AppHandle) -> Result<(), String> {
    let pubkey = app
        .config()
        .plugins
        .0
        .get("updater")
        .and_then(|config| config.get("pubkey"))
        .and_then(|value| value.as_str())
        .unwrap_or("");
    if is_real_pubkey(pubkey) {
        Ok(())
    } else {
        Err(NOT_CONFIGURED.into())
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The stamp file's path, or None when nothing is written (DBLENS_STORE=memory).
fn stamp_path() -> Option<std::path::PathBuf> {
    if std::env::var("DBLENS_STORE").as_deref() == Ok("memory") {
        return None;
    }
    Store::default_dir().ok().map(|dir| dir.join(STAMP_FILE))
}

fn read_last_check() -> Option<u64> {
    let text = std::fs::read_to_string(stamp_path()?).ok()?;
    text.trim().parse().ok()
}

fn remember_check(state: &UpdaterState) {
    let now = now_secs();
    if let Ok(mut last) = state.last_check.lock() {
        *last = Some(now);
    }
    if let Some(path) = stamp_path() {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, now.to_string());
    }
}

/// Whether a background check is due: the setting is on, and the last check is a day old or more.
fn background_check_due(app: &AppHandle, state: &UpdaterState) -> bool {
    if !settings::current(app).check_for_updates {
        return false;
    }
    let last = state
        .last_check
        .lock()
        .ok()
        .and_then(|last| *last)
        .or_else(read_last_check);
    match last {
        Some(at) => now_secs().saturating_sub(at) >= EVERY.as_secs(),
        None => true,
    }
}

/// Asks the endpoints for a newer release. Holds the pending slot for the whole
/// check, so a second check or an install waits for it.
async fn look(app: &AppHandle, state: &UpdaterState) -> Result<UpdateInfo, String> {
    let mut pending = state.pending.lock().await;
    configured(app)?;
    let found = app
        .updater()
        .map_err(|e| e.to_string())?
        .check()
        .await
        .map_err(|e| e.to_string())?;
    remember_check(state);
    let Some(update) = found else {
        *pending = None;
        return Ok(UpdateInfo::default());
    };
    let info = UpdateInfo {
        available: true,
        version: Some(update.version.clone()),
        notes: update.body.clone(),
    };
    // Automatic updates download now and install when the app quits.
    let bytes = if settings::current(app).update_automatically {
        match update.download(|_, _| {}, || {}).await {
            Ok(bytes) => Some(bytes),
            Err(e) => {
                eprintln!("update download failed: {e}");
                None
            }
        }
    } else {
        None
    };
    *pending = Some(Pending { update, bytes });
    Ok(info)
}

/// Checks now (Settings > Updates > Check Now, the Help menu). Tells every window when an update is found.
#[tauri::command]
pub async fn check_for_updates(app: AppHandle) -> Result<UpdateInfo, String> {
    let state = app.state::<UpdaterState>();
    let info = look(&app, &state).await?;
    if info.available {
        let _ = app.emit(AVAILABLE_EVENT, &info);
    }
    Ok(info)
}

/// Installs the update the last check found, then closes the windows (their
/// unsaved edits are asked about first, as with Quit) and relaunches the app.
/// On Windows the installer takes over and the app exits by itself.
#[tauri::command]
pub async fn install_update(app: AppHandle) -> Result<(), String> {
    let state = app.state::<UpdaterState>();
    let mut pending = state.pending.lock().await;
    let Some(Pending { update, bytes }) = pending.take() else {
        return Err("No update to install: check for updates first.".into());
    };
    let bytes = match bytes {
        Some(bytes) => bytes,
        None => update
            .download(|_, _| {}, || {})
            .await
            .map_err(|e| e.to_string())?,
    };
    // Verifies the signature again, then hands the package to the OS.
    update.install(&bytes).map_err(|e| e.to_string())?;
    drop(pending);
    state.restart_after_exit.store(true, Ordering::SeqCst);
    close_guard::quit(&app);
    Ok(())
}

/// Starts the background check: a little after start, then hourly, and it
/// checks only when a day has passed since the last check.
pub fn start(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_CHECK_DELAY).await;
        loop {
            let state = app.state::<UpdaterState>();
            if configured(&app).is_ok() && background_check_due(&app, &state) {
                match look(&app, &state).await {
                    Ok(info) if info.available => {
                        let _ = app.emit(AVAILABLE_EVENT, &info);
                    }
                    Ok(_) => {}
                    Err(e) => eprintln!("background update check: {e}"),
                }
            }
            tokio::time::sleep(POLL).await;
        }
    });
}

/// Called when the app is exiting (RunEvent::Exit). Installs an automatic
/// update that was downloaded but not installed yet, and relaunches after
/// install_update. Never waits: a lock held by a running check is skipped.
pub fn on_exit(app: &AppHandle) {
    let state = app.state::<UpdaterState>();
    let restart = state.restart_after_exit.load(Ordering::SeqCst);
    if let Ok(mut pending) = state.pending.try_lock() {
        if let Some(Pending {
            update,
            bytes: Some(bytes),
        }) = pending.take()
        {
            if let Err(e) = update.install(&bytes) {
                eprintln!("installing the update on exit failed: {e}");
            }
        }
    }
    if restart {
        tauri::process::restart(&app.env());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_placeholder_is_not_a_key() {
        assert!(!is_real_pubkey("REPLACE_WITH_UPDATER_PUBLIC_KEY"));
        assert!(!is_real_pubkey(""));
        assert!(!is_real_pubkey("   "));
    }

    #[test]
    fn a_minisign_key_is_a_key() {
        let text = "untrusted comment: minisign public key: 0123456789ABCDEF\nRWQabcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJ=\n";
        let encoded = base64::engine::general_purpose::STANDARD.encode(text);
        assert!(is_real_pubkey(&encoded));
        assert!(is_real_pubkey(&format!("  {encoded}\n")));
    }

    #[test]
    fn no_info_means_nothing_available() {
        assert_eq!(
            UpdateInfo::default(),
            UpdateInfo {
                available: false,
                version: None,
                notes: None
            }
        );
    }
}
