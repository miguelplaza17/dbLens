//! Linux only: where GTK needs a nudge so the native parts of a window match
//! the page.
//!
//! - The menu bar follows Settings > Appearance. tao sets
//!   `gtk-application-prefer-dark-theme`, but GTK only honors it when the
//!   configured theme (`gtk-theme-name`) exists: a missing theme (a settings.ini
//!   naming one that isn't installed, a bare X server) falls back to plain
//!   Adwaita *without* the dark variant, so the menu bar stayed light. Then the
//!   built-in Adwaita, which has both variants, is used instead.
//! - The run keys (Ctrl+Enter, with Shift or Alt) go to the page before the menu
//!   accelerators. GTK activates an accelerator in the UI process at once, while
//!   the keys typed just before it may still be queued for the web process, so
//!   the menu's Run read a stale editor text (half a query). Sent to the page,
//!   the key arrives after the text it follows; when the page leaves it
//!   unhandled, WebKit hands it back and the menu accelerator runs as before.

use std::cell::Cell;
use std::path::Path;
use std::rc::Rc;

use gtk::gdk::{keys::constants as key, ModifierType};
use gtk::gio::{self, ResourceLookupFlags};
use gtk::glib::{self, Propagation};
use gtk::prelude::*;

/// GTK's own theme, compiled into the library with its dark variant.
const BUILT_IN: &str = "Adwaita";

/// Makes the native menu bar light or dark (None: as the system says). Call on
/// the main thread.
pub fn follow_theme(theme: Option<tauri::Theme>) {
    let Some(settings) = gtk::Settings::default() else {
        return;
    };
    let configured = settings.gtk_theme_name();
    if !configured.as_deref().is_some_and(theme_installed) {
        settings.set_gtk_theme_name(Some(BUILT_IN));
    }
    if let Some(theme) = theme {
        settings.set_gtk_application_prefer_dark_theme(theme == tauri::Theme::Dark);
    }
}

/// Whether GTK 3 can load the theme `name`: built in, or a `gtk-3.x` folder in
/// one of the theme folders GTK searches.
fn theme_installed(name: &str) -> bool {
    if name.is_empty() || name.contains('/') {
        return false;
    }
    let resource = format!("/org/gtk/libgtk/theme/{name}/gtk.css");
    if gio::resources_get_info(&resource, ResourceLookupFlags::NONE).is_ok() {
        return true;
    }
    let mut roots = vec![
        glib::user_data_dir().join("themes"),
        glib::home_dir().join(".themes"),
    ];
    roots.extend(
        glib::system_data_dirs()
            .into_iter()
            .map(|d| d.join("themes")),
    );
    roots.push("/usr/share/themes".into());
    roots.iter().any(|root| has_gtk3(&root.join(name)))
}

fn has_gtk3(theme: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(theme) else {
        return false;
    };
    entries.flatten().any(|entry| {
        entry.file_name().to_string_lossy().starts_with("gtk-3.")
            && entry.path().join("gtk.css").is_file()
    })
}

/// Ctrl+Enter, Ctrl+Shift+Enter, Ctrl+Alt+Enter: Run, Run All or Nothing, Rehearse.
fn is_run_key(event: &gtk::gdk::EventKey) -> bool {
    let k = event.keyval();
    (k == key::Return || k == key::KP_Enter || k == key::ISO_Enter)
        && event.state().contains(ModifierType::CONTROL_MASK)
}

/// Sends the run keys to the focused web view before the window's menu
/// accelerators see them (see the module comment).
pub fn run_keys_to_page(window: &tauri::WebviewWindow) {
    let Ok(gtk_window) = window.gtk_window() else {
        return;
    };
    // The key we passed to the page: when WebKit hands it back unhandled, it
    // comes here again and goes the usual way (accelerator, then focus).
    let passed: Rc<Cell<Option<(u32, u32)>>> = Rc::default();
    gtk_window.connect_key_press_event(move |window, event| {
        if !is_run_key(event) {
            return Propagation::Proceed;
        }
        let id = (event.time(), *event.keyval());
        if passed.replace(None) == Some(id) {
            return Propagation::Proceed;
        }
        let Some(focus) = window.focused_widget() else {
            return Propagation::Proceed;
        };
        if focus.type_().name() != "WebKitWebView" {
            return Propagation::Proceed;
        }
        passed.set(Some(id));
        if focus.event(event) {
            Propagation::Stop
        } else {
            passed.set(None);
            Propagation::Proceed
        }
    });
}
