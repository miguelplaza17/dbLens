//! Unsaved grid edits live only in the front end, so closing a window (its
//! close button, Close Window, Quit) asks that window first.
//!
//! A page that is listening calls `close_guard_ready`; from then on a close
//! request for its window is held, and the page gets `CLOSE_EVENT`. It saves,
//! discards or keeps the edits (asking the user when there are any) and calls
//! `close_confirmed` to let the window go. A page that never registered (still
//! loading, crashed, a reload in progress) is closed at once: nothing can be
//! lost that it would know about.

use std::collections::HashSet;
use std::sync::Mutex;

use tauri::{AppHandle, Emitter, EventTarget, Manager, WebviewWindow, Window};

/// What the front end listens to: "close this window when you are done".
pub const CLOSE_EVENT: &str = "dblens-close-requested";

#[derive(Default)]
pub struct CloseGuard {
    /// Windows whose page answers close requests.
    listening: Mutex<HashSet<String>>,
    /// Windows that may close now (their page said so).
    confirmed: Mutex<HashSet<String>>,
}

impl CloseGuard {
    fn listening(&self, label: &str) -> bool {
        self.listening
            .lock()
            .map(|l| l.contains(label))
            .unwrap_or(false)
    }

    /// The page is loading anew (or went away): it answers nothing until it
    /// registers again.
    pub fn forget(&self, label: &str) {
        if let Ok(mut l) = self.listening.lock() {
            l.remove(label);
        }
        if let Ok(mut c) = self.confirmed.lock() {
            c.remove(label);
        }
    }

    /// Takes the window's confirmation, if it has one.
    fn take_confirmed(&self, label: &str) -> bool {
        self.confirmed
            .lock()
            .map(|mut c| c.remove(label))
            .unwrap_or(false)
    }
}

/// A close request for `window` (`WindowEvent::CloseRequested`): true when it
/// must wait for its page, which has been asked.
pub fn hold_close(window: &Window) -> bool {
    let guard = window.state::<CloseGuard>();
    let label = window.label();
    if guard.take_confirmed(label) || !guard.listening(label) {
        return false;
    }
    window
        .emit_to(EventTarget::webview_window(label), CLOSE_EVENT, ())
        .is_ok()
}

/// Quit: every window is asked as if it were closed; the app ends when the
/// last one goes. A window whose user keeps the edits stays open.
pub fn quit(app: &AppHandle) {
    let windows = app.webview_windows();
    if windows.is_empty() {
        app.exit(0);
        return;
    }
    let guard = app.state::<CloseGuard>();
    for (label, window) in windows {
        if guard.listening(&label) {
            let _ = app.emit_to(EventTarget::webview_window(&label), CLOSE_EVENT, ());
        } else {
            let _ = window.destroy();
        }
    }
}

/// The page answers close requests from now on.
#[tauri::command]
pub fn close_guard_ready(window: WebviewWindow, guard: tauri::State<'_, CloseGuard>) {
    if let Ok(mut l) = guard.listening.lock() {
        l.insert(window.label().to_string());
    }
}

/// The page is done with its edits: the window closes.
#[tauri::command]
pub fn close_confirmed(window: WebviewWindow, guard: tauri::State<'_, CloseGuard>) {
    if let Ok(mut c) = guard.confirmed.lock() {
        c.insert(window.label().to_string());
    }
    let _ = window.close();
}
