//! dbLens in the tray (the menu bar on macOS, the notification area on
//! Windows, the tray on Linux). With "Keep dbLens in the tray" on, closing the
//! last window leaves the app running and the tray icon brings a window back;
//! Quit (the menu, the tray) still ends it. Mirrors `MenuBar.swift`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Wry};

const TRAY_ID: &str = "dblens-tray";
/// (id, English text); `set_menu_labels` translates them with "tray:<id>".
const ITEMS: [(&str, &str); 3] = [
    ("open", "Open dbLens"),
    ("settings", "Settings…"),
    ("updates", "Check for Updates…"),
];
const QUIT: (&str, &str) = ("quit", "Quit");

#[derive(Default)]
pub struct TrayState {
    /// Quit was asked for: the app ends when its last window is gone.
    quitting: AtomicBool,
    items: Mutex<HashMap<&'static str, MenuItem<Wry>>>,
}

impl TrayState {
    pub fn begin_quit(&self) {
        self.quitting.store(true, Ordering::SeqCst);
    }
}

/// Whether the app should outlive its last window.
pub fn keeps_running(app: &AppHandle) -> bool {
    crate::settings::current(app).keep_in_tray
        && !app.state::<TrayState>().quitting.load(Ordering::SeqCst)
}

/// Builds the tray icon (shown or not as the setting says).
pub fn install(app: &AppHandle) -> tauri::Result<()> {
    let state = app.state::<TrayState>();
    let menu = Menu::new(app)?;
    {
        let mut items = state.items.lock().unwrap_or_else(|e| e.into_inner());
        for (id, text) in ITEMS {
            let item = MenuItem::with_id(app, format!("tray-{id}"), text, true, None::<&str>)?;
            menu.append(&item)?;
            items.insert(id, item);
        }
    }
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    let quit = MenuItem::with_id(app, format!("tray-{}", QUIT.0), QUIT.1, true, None::<&str>)?;
    menu.append(&quit)?;
    state
        .items
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(QUIT.0, quit);
    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("dbLens")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| on_menu(app, event.id().as_ref()))
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    let tray = builder.build(app)?;
    tray.set_visible(crate::settings::current(app).keep_in_tray)?;
    Ok(())
}

/// The setting changed: show or hide the icon.
pub fn apply(app: &AppHandle, keep_in_tray: bool) {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_visible(keep_in_tray);
    }
}

/// The UI language's wording; ids are "tray:open", "tray:settings",
/// "tray:updates" and "tray:quit".
pub fn set_labels(app: &AppHandle, labels: &HashMap<String, String>) {
    let state = app.state::<TrayState>();
    let items = state.items.lock().unwrap_or_else(|e| e.into_inner());
    for (id, item) in items.iter() {
        if let Some(text) = labels.get(&format!("tray:{id}")) {
            let _ = item.set_text(text);
        }
    }
}

/// Brings a window forward, opening one when none is left.
pub fn show(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
    let windows = app.webview_windows();
    let main = windows
        .iter()
        .find(|(label, _)| label.as_str() != crate::settings::WINDOW_LABEL)
        .map(|(_, w)| w.clone());
    match main {
        Some(window) => {
            let _ = window.unminimize();
            let _ = window.show();
            let _ = window.set_focus();
        }
        None => {
            let _ = crate::windows::open_new(app, None);
        }
    }
}

/// The last window is gone and the app stays: on macOS the Dock icon goes too.
pub fn windows_gone(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    if app.webview_windows().is_empty() {
        let _ = app.set_activation_policy(tauri::ActivationPolicy::Accessory);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = app;
}

fn on_menu(app: &AppHandle, id: &str) {
    match id {
        "tray-open" => show(app),
        "tray-settings" => {
            #[cfg(target_os = "macos")]
            let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
            let title = app
                .state::<TrayState>()
                .items
                .lock()
                .ok()
                .and_then(|items| items.get("settings").and_then(|i| i.text().ok()))
                .unwrap_or_else(|| "Settings".into());
            let _ = crate::settings::open_settings(app.clone(), title.trim_end_matches('…').into());
        }
        "tray-updates" => {
            show(app);
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                let _ = crate::updater::check_for_updates(app).await;
            });
        }
        "tray-quit" => crate::close_guard::quit(app),
        _ => {}
    }
}
