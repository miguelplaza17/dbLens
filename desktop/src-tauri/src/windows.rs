//! More than one window. Each window is its own web view with its own
//! sessions (`AppState` keys them by the web view's label and closes them when
//! the window is destroyed, see lib.rs). Window size and position are kept per
//! label by tauri-plugin-window-state.

use std::collections::HashMap;
use std::sync::Mutex;

use tauri::{AppHandle, Manager, PhysicalPosition, WebviewUrl, WebviewWindowBuilder};

/// Database files a window should open once its front end is up (a second
/// launch with a file argument, or "open with" at start).
#[derive(Default)]
pub struct PendingOpen(Mutex<HashMap<String, Vec<String>>>);

/// The first label "w2", "w3"… not in use. Labels are reused on purpose: the
/// window-state plugin keeps one saved size per label, so random labels would
/// never restore and would grow the file.
pub fn free_label(taken: &[String]) -> String {
    (2..)
        .map(|n| format!("w{n}"))
        .find(|l| !taken.contains(l))
        .expect("an unused label")
}

/// Whether a command-line argument is a database file we can open.
pub fn is_database_path(arg: &str) -> bool {
    let lower = arg.to_lowercase();
    [".db", ".sqlite", ".sqlite3", ".db3"]
        .iter()
        .any(|e| lower.ends_with(e))
}

/// The database files among a launch's arguments (the first one is the program).
pub fn database_args(args: &[String]) -> Vec<String> {
    database_args_in(args, None)
}

/// Like `database_args`, with relative paths made absolute against the
/// directory the launch ran in (a second launch runs in another process).
pub fn database_args_in(args: &[String], cwd: Option<&str>) -> Vec<String> {
    args.iter()
        .skip(1)
        .filter(|a| is_database_path(a))
        .map(|a| match cwd {
            Some(dir) if !dir.is_empty() && !std::path::Path::new(a).is_absolute() => {
                std::path::Path::new(dir)
                    .join(a)
                    .to_string_lossy()
                    .into_owned()
            }
            _ => a.clone(),
        })
        .collect()
}

/// Opens another window. `open` queues database files for it. Returns its label.
pub fn open_new(app: &AppHandle, open: Option<Vec<String>>) -> tauri::Result<String> {
    let taken: Vec<String> = app.webview_windows().into_keys().collect();
    let label = free_label(&taken);
    if let Some(paths) = open.filter(|p| !p.is_empty()) {
        if let Ok(mut map) = app.state::<PendingOpen>().0.lock() {
            map.insert(label.clone(), paths);
        }
    }
    let mut builder = WebviewWindowBuilder::new(app, &label, WebviewUrl::App("index.html".into()))
        .title("dbLens")
        .inner_size(1280.0, 800.0)
        .min_inner_size(800.0, 500.0)
        .theme(Some(tauri::Theme::Dark));
    // A new window lands a little down and right of the one in front, unless
    // the plugin restores a saved place for this label.
    if let Some(front) = app
        .webview_windows()
        .into_values()
        .find(|w| w.is_focused().unwrap_or(false))
    {
        if let (Ok(p), Ok(scale)) = (front.outer_position(), front.scale_factor()) {
            let step = (28.0 * scale) as i32;
            let PhysicalPosition { x, y } = p;
            builder = builder.position((x + step) as f64 / scale, (y + step) as f64 / scale);
        }
    }
    let window = builder.build()?;
    #[cfg(target_os = "linux")]
    crate::linux_gtk::run_keys_to_page(&window);
    let _ = window.set_focus();
    Ok(label)
}

/// A second launch: show the running app. With database files, open them in a
/// new window; otherwise raise the window in front (or the first one).
pub fn second_launch(app: &AppHandle, args: &[String], cwd: &str) {
    let files = database_args_in(args, Some(cwd));
    if !files.is_empty() {
        let _ = open_new(app, Some(files));
        return;
    }
    let windows = app.webview_windows();
    let target = windows
        .values()
        .find(|w| w.is_focused().unwrap_or(false))
        .or_else(|| windows.get("main"))
        .or_else(|| windows.values().next());
    if let Some(w) = target {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

#[tauri::command]
pub fn new_window(app: AppHandle) -> Result<String, String> {
    open_new(&app, None).map_err(|e| e.to_string())
}

/// The files queued for this window; asked once when the front end starts.
#[tauri::command]
pub fn take_pending_open(window: tauri::WebviewWindow) -> Vec<String> {
    let state = window.state::<PendingOpen>();
    state
        .0
        .lock()
        .ok()
        .and_then(|mut m| m.remove(window.label()))
        .unwrap_or_default()
}

/// Queues the files in this process's own arguments for the first window.
pub fn queue_launch_args(app: &AppHandle) {
    let args: Vec<String> = std::env::args().collect();
    let files = database_args(&args);
    if !files.is_empty() {
        if let Ok(mut map) = app.state::<PendingOpen>().0.lock() {
            map.insert("main".into(), files);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_start_at_w2_and_reuse_gaps() {
        assert_eq!(free_label(&["main".into()]), "w2");
        assert_eq!(free_label(&["main".into(), "w2".into(), "w4".into()]), "w3");
        assert_eq!(free_label(&["main".into(), "w2".into(), "w3".into()]), "w4");
    }

    #[test]
    fn database_files_are_recognised_by_extension() {
        for ok in ["a.db", "A.SQLITE", "/x/y.sqlite3", "C:\\d\\z.db3"] {
            assert!(is_database_path(ok), "{ok}");
        }
        for no in ["a.txt", "db", "--flag", "notes.db.bak"] {
            assert!(!is_database_path(no), "{no}");
        }
        let args: Vec<String> = ["dblens", "/a/b.db", "--x", "c.txt", "d.sqlite"]
            .map(String::from)
            .to_vec();
        assert_eq!(database_args(&args), vec!["/a/b.db", "d.sqlite"]);
        // The program itself is never a file to open.
        assert!(database_args(&["app.db".to_string()]).is_empty());
    }

    #[test]
    fn relative_database_paths_join_the_callers_directory() {
        let args: Vec<String> = ["dblens", "data/app.db", "/abs/b.db", "x.txt"]
            .map(String::from)
            .to_vec();
        let got = database_args_in(&args, Some("/work/dir"));
        assert_eq!(
            got,
            vec![
                std::path::Path::new("/work/dir")
                    .join("data/app.db")
                    .to_string_lossy(),
                "/abs/b.db".into()
            ]
        );
        assert_eq!(database_args_in(&args, None)[0], "data/app.db");
    }
}
