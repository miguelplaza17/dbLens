//! App settings: what the Settings window changes (Sources/dbLens/SettingsView.swift,
//! UI/Theme.swift, UI/AgentSettings.swift). One JSON file, `settings.json`, in
//! the data folder (`Store::default_dir`, so `DBLENS_STORE_DIR` moves it too);
//! with `DBLENS_STORE=memory` nothing is written. Every change is sent to all
//! windows as `SETTINGS_EVENT`, so the theme and accent change everywhere at once.
//!
//! Other parts read the current values with `current(app)`: the updater
//! (`check_for_updates`, `update_automatically`). "Let agents add and remove
//! connections" is not here: the MCP server runs as its own process and reads
//! it from the store (`StoreSnapshot::agents_manage_connections`).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

use crate::store::Store;

/// The event every window gets after a change; the payload is the whole `Settings`.
pub const SETTINGS_EVENT: &str = "settings-changed";
/// The Settings window's label (there is one at most).
pub const WINDOW_LABEL: &str = "settings";
/// The Settings window's width; its height follows the shown tab (`fit_settings`).
pub const WINDOW_WIDTH: f64 = 720.0;
const MIN_HEIGHT: f64 = 240.0;
const MAX_HEIGHT: f64 = 860.0;

/// The accent ids, as `Theme.all` in UI/Theme.swift. Their colors are in
/// `src/settings/accents.css`; "system" keeps the theme's own accent.
pub const ACCENTS: [&str; 12] = [
    "system", "elephant", "blue", "indigo", "violet", "pink", "red", "orange", "yellow", "mint",
    "teal", "graphite",
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Appearance {
    /// Follow the OS's light or dark mode.
    System,
    Light,
    /// The default, as the app was before there was a choice.
    #[default]
    Dark,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub appearance: Appearance,
    /// One of `ACCENTS`.
    pub accent: String,
    /// Settings > Updates > "Check for updates automatically".
    pub check_for_updates: bool,
    /// Settings > Updates > "Update automatically".
    pub update_automatically: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            appearance: Appearance::Dark,
            accent: "system".into(),
            check_for_updates: true,
            update_automatically: false,
        }
    }
}

impl Settings {
    /// Reads `path`; a missing or damaged file gives the defaults (the next
    /// change writes a good one). Unknown accents fall back to "system".
    pub fn load(path: &Path) -> Settings {
        let mut settings = std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Settings>(&bytes).ok())
            .unwrap_or_default();
        if !ACCENTS.contains(&settings.accent.as_str()) {
            settings.accent = "system".into();
        }
        settings
    }

    /// Writes `path` through a temp file and a rename, so a crash never leaves half a file.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let tmp = path.with_extension("json.tmp");
        let text = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, path).map_err(|e| e.to_string())
    }

    /// These settings with the fields in `patch` (a JSON object, camelCase) changed.
    /// Unknown fields and wrong types are refused, so a typo never saves silently.
    pub fn patched(&self, patch: &serde_json::Value) -> Result<Settings, String> {
        let Some(fields) = patch.as_object() else {
            return Err("Settings: expected an object".into());
        };
        let mut merged = serde_json::to_value(self).map_err(|e| e.to_string())?;
        let target = merged
            .as_object_mut()
            .expect("settings serialize to an object");
        for (key, value) in fields {
            if !target.contains_key(key) {
                return Err(format!("Settings: unknown setting {key}"));
            }
            target.insert(key.clone(), value.clone());
        }
        let next: Settings =
            serde_json::from_value(merged).map_err(|e| format!("Settings: {e}"))?;
        if !ACCENTS.contains(&next.accent.as_str()) {
            return Err(format!("Settings: unknown accent {}", next.accent));
        }
        Ok(next)
    }

    pub(crate) fn native_theme(&self) -> Option<tauri::Theme> {
        match self.appearance {
            Appearance::System => None,
            Appearance::Light => Some(tauri::Theme::Light),
            Appearance::Dark => Some(tauri::Theme::Dark),
        }
    }
}

/// The settings in memory, and the file they live in (None: memory only).
pub struct SettingsState {
    path: Option<PathBuf>,
    current: Mutex<Settings>,
}

impl SettingsState {
    /// The data folder's `settings.json`, or memory only with `DBLENS_STORE=memory`.
    pub fn from_env() -> SettingsState {
        let path = if std::env::var("DBLENS_STORE").as_deref() == Ok("memory") {
            None
        } else {
            Store::default_dir().ok().map(|d| d.join("settings.json"))
        };
        SettingsState::at(path)
    }

    pub fn at(path: Option<PathBuf>) -> SettingsState {
        let current = path.as_deref().map(Settings::load).unwrap_or_default();
        SettingsState {
            path,
            current: Mutex::new(current),
        }
    }

    pub fn get(&self) -> Settings {
        self.current.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// Applies `patch`, saves, and returns the new settings.
    pub fn update(&self, patch: &serde_json::Value) -> Result<Settings, String> {
        let mut guard = self.current.lock().map_err(|e| e.to_string())?;
        let next = guard.patched(patch)?;
        if let Some(path) = &self.path {
            next.save(path)?;
        }
        *guard = next.clone();
        Ok(next)
    }
}

/// The current settings, for other modules (MCP server, updater).
pub fn current(app: &AppHandle) -> Settings {
    app.try_state::<SettingsState>()
        .map(|s| s.get())
        .unwrap_or_default()
}

/// What the Settings window shows besides the settings themselves.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsInfo {
    /// The app's version (tauri.conf.json).
    pub version: String,
    /// This program's path, for `<path> --mcp` in agent apps.
    pub executable: String,
    /// The data folder; None when nothing is kept on disk (`DBLENS_STORE=memory`).
    pub data_folder: Option<String>,
    /// "macos", "windows" or "linux".
    pub os: String,
}

/// Linux: the GTK menu bar takes the same theme as the windows (linux_gtk.rs).
/// Elsewhere the window theme already covers it.
fn follow_menu_theme(app: &AppHandle, settings: &Settings) {
    #[cfg(target_os = "linux")]
    {
        let theme = settings.native_theme();
        let _ = app.run_on_main_thread(move || crate::linux_gtk::follow_theme(theme));
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (app, settings);
}

// MARK: - Commands

/// The settings; also gives the calling window the native (title bar) theme.
#[tauri::command]
pub fn get_settings(window: WebviewWindow, state: tauri::State<SettingsState>) -> Settings {
    let settings = state.get();
    let _ = window.set_theme(settings.native_theme());
    follow_menu_theme(window.app_handle(), &settings);
    settings
}

/// Changes the fields in `patch`, saves them and tells every window.
#[tauri::command]
pub fn set_settings(
    app: AppHandle,
    state: tauri::State<SettingsState>,
    patch: serde_json::Value,
) -> Result<Settings, String> {
    let settings = state.update(&patch)?;
    for window in app.webview_windows().values() {
        let _ = window.set_theme(settings.native_theme());
    }
    follow_menu_theme(&app, &settings);
    let _ = app.emit(SETTINGS_EVENT, &settings);
    Ok(settings)
}

#[tauri::command]
pub fn settings_info(app: AppHandle) -> SettingsInfo {
    let memory = std::env::var("DBLENS_STORE").as_deref() == Ok("memory");
    SettingsInfo {
        version: app.package_info().version.to_string(),
        executable: std::env::current_exe()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "dblens".into()),
        data_folder: if memory {
            None
        } else {
            Store::default_dir()
                .ok()
                .map(|d| d.to_string_lossy().into_owned())
        },
        os: std::env::consts::OS.into(),
    }
}

/// Opens the Settings window, or brings it to the front. `title` is the
/// window title in the UI's language.
#[tauri::command]
pub fn open_settings(app: AppHandle, title: String) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(WINDOW_LABEL) {
        let _ = window.unminimize();
        let _ = window.show();
        return window.set_focus().map_err(|e| e.to_string());
    }
    let settings = current(&app);
    let window =
        WebviewWindowBuilder::new(&app, WINDOW_LABEL, WebviewUrl::App("index.html".into()))
            .title(title)
            .inner_size(WINDOW_WIDTH, 480.0)
            // Resizable, but held at the fitted size by min = max (`fit_settings`):
            // GTK ignores resizes of a window that isn't resizable.
            .min_inner_size(WINDOW_WIDTH, MIN_HEIGHT)
            .max_inner_size(WINDOW_WIDTH, MAX_HEIGHT)
            .maximizable(false)
            .minimizable(false)
            .theme(settings.native_theme())
            .center()
            .build()
            .map_err(|e| e.to_string())?;
    // The menu bar belongs to the database windows (on macOS it is the app's anyway).
    #[cfg(not(target_os = "macos"))]
    let _ = window.remove_menu();
    window.set_focus().map_err(|e| e.to_string())
}

/// Sizes the Settings window to its content: the page measures itself and
/// sends the height it needs (CSS pixels), so there is never a scroll bar
/// with nothing to scroll. Past `MAX_HEIGHT` the page scrolls.
#[tauri::command]
pub fn fit_settings(window: WebviewWindow, height: f64) -> Result<(), String> {
    if window.label() != WINDOW_LABEL || !height.is_finite() {
        return Ok(());
    }
    let height = height.clamp(MIN_HEIGHT, MAX_HEIGHT).round();
    let size = tauri::LogicalSize::new(WINDOW_WIDTH, height);
    let err = |e: tauri::Error| e.to_string();
    // Loosen the limits, resize, then pin the new size (the requests run in order).
    window
        .set_min_size(Some(tauri::LogicalSize::new(WINDOW_WIDTH, MIN_HEIGHT)))
        .map_err(err)?;
    window
        .set_max_size(Some(tauri::LogicalSize::new(WINDOW_WIDTH, MAX_HEIGHT)))
        .map_err(err)?;
    window.set_size(size).map_err(err)?;
    window.set_min_size(Some(size)).map_err(err)?;
    window.set_max_size(Some(size)).map_err(err)
}

/// Opens the data folder in the file manager (Explorer, Finder, Files…).
#[tauri::command]
pub fn show_data_folder() -> Result<(), String> {
    if std::env::var("DBLENS_STORE").as_deref() == Ok("memory") {
        return Err("Nothing is kept on disk in this session (DBLENS_STORE=memory).".into());
    }
    let dir = Store::default_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    tauri_plugin_opener::open_path(&dir, None::<&str>).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn defaults_and_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let state = SettingsState::at(Some(path.clone()));
        assert_eq!(state.get(), Settings::default());
        let next = state
            .update(&json!({ "appearance": "light", "accent": "mint" }))
            .unwrap();
        assert_eq!(next.appearance, Appearance::Light);
        assert_eq!(next.accent, "mint");
        assert!(next.check_for_updates, "untouched fields keep their value");
        let again = SettingsState::at(Some(path.clone()));
        assert_eq!(again.get(), next);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"checkForUpdates\": true"), "{text}");
    }

    #[test]
    fn refuses_unknown_fields_types_and_accents() {
        let s = Settings::default();
        assert!(s.patched(&json!({ "acent": "mint" })).is_err());
        assert!(s.patched(&json!({ "accent": "chartreuse" })).is_err());
        assert!(s.patched(&json!({ "appearance": "sepia" })).is_err());
        assert!(s.patched(&json!({ "checkForUpdates": "yes" })).is_err());
        assert!(s.patched(&json!(["accent"])).is_err());
        assert!(s
            .patched(&json!({ "agentsManageConnections": true }))
            .is_err());
        let ok = s.patched(&json!({ "updateAutomatically": true })).unwrap();
        assert!(ok.update_automatically);
    }

    #[test]
    fn damaged_or_old_files_give_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(Settings::load(&path), Settings::default());
        std::fs::write(&path, r#"{"accent":"gone","appearance":"system"}"#).unwrap();
        let s = Settings::load(&path);
        assert_eq!(s.accent, "system");
        assert_eq!(s.appearance, Appearance::System);
        assert!(s.check_for_updates);
    }

    #[test]
    fn memory_state_never_writes() {
        let state = SettingsState::at(None);
        state.update(&json!({ "accent": "teal" })).unwrap();
        assert_eq!(state.get().accent, "teal");
    }

    #[test]
    fn every_accent_has_colors_in_the_css() {
        let css = include_str!("../../src/settings/accents.css");
        let ts = include_str!("../../src/settings/accents.ts");
        for id in ACCENTS {
            assert!(ts.contains(&format!("\"{id}\"")), "accents.ts lacks {id}");
            if id != "system" {
                assert!(
                    css.contains(&format!("html.accent-{id} ")),
                    "accents.css lacks {id}"
                );
                assert!(
                    css.contains(&format!(".swatch-{id} ")),
                    "accents.css lacks the {id} swatch"
                );
            }
        }
    }
}
