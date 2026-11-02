//! The native menu bar. Its rows come from `src/shortcuts.json`, the same file
//! the front end's key handler reads (`src/shortcuts.ts`), so a menu
//! accelerator and a keyboard shortcut cannot disagree. A click sends the
//! action id to the focused window as a `menu-action` event; the front end runs
//! it through the same dispatcher as a key press (`src/native/actions.ts`).
//!
//! The layout and the enabled rules are plain data and functions so they can be
//! tested without a window.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use serde::Deserialize;
use tauri::menu::{
    AboutMetadataBuilder, Menu, MenuItem, MenuItemBuilder, PredefinedMenuItem, Submenu,
    SubmenuBuilder,
};
use tauri::{AppHandle, Emitter, EventTarget, Manager, Wry};

/// The shared table, compiled in. `inFields` is only the front end's concern.
const TABLE: &str = include_str!("../../src/shortcuts.json");

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Spec {
    pub action: String,
    /// "Mod+Shift+N"; empty when the action has no key.
    pub keys: String,
    pub title: String,
    /// The menu's wording when it differs from `title`.
    pub menu: Option<String>,
    /// Only on this OS ("mac"); every OS when absent.
    pub os: Option<String>,
}

impl Spec {
    /// Whether the row belongs in the menu on this OS.
    pub fn on(&self, os: Os) -> bool {
        match self.os.as_deref() {
            None => true,
            Some("mac") => os == Os::Mac,
            Some(_) => false,
        }
    }
}

pub fn table() -> &'static [Spec] {
    static CELL: OnceLock<Vec<Spec>> = OnceLock::new();
    CELL.get_or_init(|| serde_json::from_str(TABLE).expect("src/shortcuts.json is valid"))
}

pub fn spec(action: &str) -> Option<&'static Spec> {
    table().iter().find(|s| s.action == action)
}

/// The accelerator for the menu, or None. "Mod" is Cmd on macOS and Ctrl
/// elsewhere. A key without a modifier ("Delete") is not a menu accelerator:
/// it would take the key away from text fields.
pub fn accelerator(keys: &str) -> Option<String> {
    if keys.is_empty() || !keys.split('+').any(|p| p == "Mod") {
        return None;
    }
    Some(
        keys.split('+')
            .map(|p| if p == "Mod" { "CmdOrCtrl" } else { p })
            .collect::<Vec<_>>()
            .join("+"),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Mac,
    Windows,
    Linux,
}

impl Os {
    pub fn current() -> Os {
        if cfg!(target_os = "macos") {
            Os::Mac
        } else if cfg!(windows) {
            Os::Windows
        } else {
            Os::Linux
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Predefined {
    About,
    Quit,
    Hide,
    HideOthers,
    ShowAll,
    Services,
    Minimize,
    Maximize,
    Fullscreen,
    Cut,
    Copy,
    Paste,
    SelectAll,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entry {
    /// An action from shortcuts.json; its click goes to the front end (or the
    /// Rust handler for window actions).
    Action(&'static str),
    /// Rows that are not in shortcuts.json. Undo and Redo are predefined only on
    /// macOS; elsewhere the front end does them on the focused field or editor.
    /// Quit is predefined everywhere but Windows and Linux.
    Native(&'static str),
    Predefined(Predefined),
    Separator,
}

pub struct Menus {
    pub title: &'static str,
    pub entries: Vec<Entry>,
}

use Entry::{Action as A, Native as E, Predefined as P, Separator as S};

/// The menu bar for an OS. macOS follows Apple's layout: the app menu holds
/// About, Settings and Quit. Windows keeps Settings under File; Linux under Edit.
pub fn layout(os: Os) -> Vec<Menus> {
    let mac = os == Os::Mac;
    let mut menus = Vec::new();
    if mac {
        menus.push(Menus {
            title: "dbLens",
            entries: vec![
                P(Predefined::About),
                S,
                A("settings"),
                S,
                P(Predefined::Services),
                S,
                P(Predefined::Hide),
                P(Predefined::HideOthers),
                P(Predefined::ShowAll),
                S,
                P(Predefined::Quit),
            ],
        });
    }
    let mut file: Vec<Entry> = vec![A("newConnection"), A("newWindow"), A("openFile")];
    if mac {
        file.push(A("importSwift"));
    }
    file.extend([S, A("disconnect"), S, A("closeWindow")]);
    if os == Os::Windows {
        file.extend([S, A("settings")]);
    }
    if !mac {
        file.extend([S, E("quit")]);
    }
    menus.push(Menus {
        title: "File",
        entries: file,
    });

    let mut edit = vec![
        E("undo"),
        E("redo"),
        S,
        P(Predefined::Cut),
        P(Predefined::Copy),
        P(Predefined::Paste),
        P(Predefined::SelectAll),
        S,
        A("copySelection"),
        A("copyRows"),
        S,
        A("newRow"),
        A("deleteRows"),
        A("setNull"),
        S,
        A("saveChanges"),
        A("revertChanges"),
    ];
    if os == Os::Linux {
        edit.extend([S, A("settings")]);
    }
    menus.push(Menus {
        title: "Edit",
        entries: edit,
    });

    let mut view = vec![
        A("toggleSidebar"),
        A("toggleSQLPanel"),
        S,
        A("filterTables"),
        A("searchRows"),
        S,
        A("previousPage"),
        A("nextPage"),
        A("freezeFirstColumn"),
        S,
        A("refresh"),
        A("toggleTheme"),
        A("perfHud"),
    ];
    if mac {
        view.extend([S, P(Predefined::Fullscreen)]);
    }
    menus.push(Menus {
        title: "View",
        entries: view,
    });

    menus.push(Menus {
        title: "Query",
        entries: vec![A("run"), A("runAtomically"), A("rehearse"), S, A("cancel")],
    });

    if mac {
        menus.push(Menus {
            title: "Window",
            entries: vec![P(Predefined::Minimize), P(Predefined::Maximize)],
        });
    }
    let mut help = vec![A("showShortcuts")];
    if !mac {
        help.extend([S, P(Predefined::About)]);
    }
    menus.push(Menus {
        title: "Help",
        entries: help,
    });
    menus
}

/// What the focused window tells the menu, so rows grey out when they can't work.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct MenuState {
    pub connected: bool,
    /// A table or collection is open (rows can be searched and paged).
    pub has_table: bool,
    pub can_insert: bool,
    pub can_edit: bool,
    pub has_selection: bool,
    pub has_result: bool,
    pub can_back: bool,
    pub can_forward: bool,
    pub sql_panel: bool,
    pub running: bool,
}

/// Whether an action's row is enabled.
pub fn enabled(action: &str, s: &MenuState) -> bool {
    match action {
        "disconnect" | "refresh" | "filterTables" => s.connected,
        "searchRows" => s.has_table,
        "previousPage" => s.can_back,
        "nextPage" => s.can_forward,
        "freezeFirstColumn" => s.has_result,
        "saveChanges" | "revertChanges" => s.can_edit,
        "newRow" => s.can_insert,
        "deleteRows" | "setNull" => s.can_edit && s.has_selection,
        "copySelection" => s.has_result && s.has_selection,
        "copyRows" => s.has_result,
        "run" | "rehearse" | "runAtomically" => s.connected && !s.running,
        "cancel" => s.running,
        _ => true,
    }
}

/// Handles to the rows whose state changes, by action id.
#[derive(Default)]
pub struct MenuHandles(Mutex<HashMap<String, MenuItem<Wry>>>);

fn label(action: &str) -> String {
    match spec(action) {
        Some(s) => s.menu.clone().unwrap_or_else(|| s.title.clone()),
        None => match action {
            "undo" => "Undo".into(),
            "redo" => "Redo".into(),
            "quit" => "Quit".into(),
            other => other.into(),
        },
    }
}

fn build_item(
    app: &AppHandle,
    handles: &mut HashMap<String, MenuItem<Wry>>,
    id: &'static str,
) -> tauri::Result<MenuItem<Wry>> {
    let mut builder = MenuItemBuilder::with_id(id, label(id));
    let keys = spec(id).map(|s| s.keys.as_str()).unwrap_or("");
    let accel = match id {
        "undo" => Some("CmdOrCtrl+Z".to_string()),
        "redo" => Some(if cfg!(windows) || cfg!(target_os = "linux") {
            "CmdOrCtrl+Y".to_string()
        } else {
            "CmdOrCtrl+Shift+Z".to_string()
        }),
        "quit" if cfg!(target_os = "linux") => Some("CmdOrCtrl+Q".to_string()),
        _ => accelerator(keys),
    };
    if let Some(a) = accel {
        builder = builder.accelerator(a);
    }
    // Nothing needing a connection works before the front end reports its state.
    let item = builder
        .enabled(enabled(id, &MenuState::default()))
        .build(app)?;
    handles.insert(id.to_string(), item.clone());
    Ok(item)
}

fn predefined(app: &AppHandle, p: Predefined) -> tauri::Result<PredefinedMenuItem<Wry>> {
    match p {
        Predefined::About => {
            let meta = AboutMetadataBuilder::new()
                .name(Some("dbLens"))
                .version(Some(env!("CARGO_PKG_VERSION")))
                .build();
            PredefinedMenuItem::about(app, Some("About dbLens"), Some(meta))
        }
        Predefined::Quit => PredefinedMenuItem::quit(app, None),
        Predefined::Hide => PredefinedMenuItem::hide(app, None),
        Predefined::HideOthers => PredefinedMenuItem::hide_others(app, None),
        Predefined::ShowAll => PredefinedMenuItem::show_all(app, None),
        Predefined::Services => PredefinedMenuItem::services(app, None),
        Predefined::Minimize => PredefinedMenuItem::minimize(app, None),
        Predefined::Maximize => PredefinedMenuItem::maximize(app, None),
        Predefined::Fullscreen => PredefinedMenuItem::fullscreen(app, None),
        Predefined::Cut => PredefinedMenuItem::cut(app, None),
        Predefined::Copy => PredefinedMenuItem::copy(app, None),
        Predefined::Paste => PredefinedMenuItem::paste(app, None),
        Predefined::SelectAll => PredefinedMenuItem::select_all(app, None),
    }
}

pub fn build(app: &AppHandle) -> tauri::Result<Menu<Wry>> {
    let os = Os::current();
    let state = app.state::<MenuHandles>();
    let mut handles = state.0.lock().expect("menu handles");
    let mut bar = Vec::new();
    for menu in layout(os) {
        let mut sub = SubmenuBuilder::new(app, menu.title);
        if menu.title == "Window" {
            sub = sub.id("window");
        }
        let mut built: Vec<Box<dyn tauri::menu::IsMenuItem<Wry>>> = Vec::new();
        for entry in menu.entries {
            match entry {
                Entry::Native(id) if os == Os::Mac => {
                    // macOS has the system Undo and Redo, which reach the web view.
                    built.push(Box::new(match id {
                        "undo" => PredefinedMenuItem::undo(app, None)?,
                        _ => PredefinedMenuItem::redo(app, None)?,
                    }));
                }
                Entry::Action(id) | Entry::Native(id) => {
                    built.push(Box::new(build_item(app, &mut handles, id)?));
                }
                Entry::Predefined(p) => built.push(Box::new(predefined(app, p)?)),
                Entry::Separator => built.push(Box::new(PredefinedMenuItem::separator(app)?)),
            }
        }
        for item in &built {
            sub = sub.item(item.as_ref());
        }
        let submenu: Submenu<Wry> = sub.build()?;
        bar.push(submenu);
    }
    let refs: Vec<&dyn tauri::menu::IsMenuItem<Wry>> = bar.iter().map(|s| s as _).collect();
    Menu::with_items(app, &refs)
}

/// Builds the menu bar and installs it. Called once from `setup`.
pub fn install(app: &AppHandle) -> tauri::Result<()> {
    app.manage(MenuHandles::default());
    let menu = build(app)?;
    app.set_menu(menu)?;
    #[cfg(target_os = "macos")]
    if let Some(window_menu) = app
        .menu()
        .and_then(|m| m.get("window"))
        .and_then(|k| k.as_submenu().cloned())
    {
        let _ = window_menu.set_as_windows_menu_for_nsapp();
    }
    Ok(())
}

/// The window a menu click is for: the focused one, else "main".
fn focused_label(app: &AppHandle) -> String {
    app.webview_windows()
        .into_iter()
        .find(|(_, w)| w.is_focused().unwrap_or(false))
        .map(|(label, _)| label)
        .unwrap_or_else(|| "main".to_string())
}

/// A menu click. Window actions are done here; every other action goes to the
/// focused window's front end.
pub fn on_event(app: &AppHandle, id: &str) {
    let label = focused_label(app);
    match id {
        "newWindow" => {
            let _ = crate::windows::open_new(app, None);
        }
        "quit" => app.exit(0),
        "closeWindow" => {
            if let Some(w) = app.get_webview_window(&label) {
                let _ = w.close();
            }
        }
        _ => {
            let _ = app.emit_to(EventTarget::webview_window(label), "menu-action", id);
        }
    }
}

/// The focused window reports what it can do; rows grey out to match.
#[tauri::command]
pub fn set_menu_state(app: AppHandle, state: MenuState) {
    let handles = app.state::<MenuHandles>();
    let Ok(map) = handles.0.lock() else { return };
    for (id, item) in map.iter() {
        let _ = item.set_enabled(enabled(id, &state));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn actions_in(os: Os) -> Vec<&'static str> {
        layout(os)
            .into_iter()
            .flat_map(|m| m.entries)
            .filter_map(|e| match e {
                Entry::Action(id) => Some(id),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn every_action_is_in_the_menu_on_every_os() {
        for os in [Os::Mac, Os::Windows, Os::Linux] {
            let in_menu: HashSet<_> = actions_in(os).into_iter().collect();
            for s in table().iter().filter(|s| s.on(os)) {
                assert!(
                    in_menu.contains(s.action.as_str()),
                    "{os:?}: {} has no menu row",
                    s.action
                );
            }
        }
    }

    #[test]
    fn every_menu_row_has_a_table_row_and_appears_once() {
        for os in [Os::Mac, Os::Windows, Os::Linux] {
            let ids = actions_in(os);
            let unique: HashSet<_> = ids.iter().collect();
            assert_eq!(ids.len(), unique.len(), "{os:?}: an action appears twice");
            for id in ids {
                let row = spec(id);
                assert!(row.is_some(), "{id} is not in shortcuts.json");
                assert!(
                    row.is_some_and(|r| r.on(os)),
                    "{os:?}: {id} is not for this OS"
                );
            }
        }
    }

    #[test]
    fn settings_sits_where_each_platform_expects_it() {
        let find = |os, title: &str| {
            layout(os)
                .into_iter()
                .find(|m| m.entries.contains(&Entry::Action("settings")))
                .map(|m| m.title == title)
        };
        assert_eq!(find(Os::Mac, "dbLens"), Some(true));
        assert_eq!(find(Os::Windows, "File"), Some(true));
        assert_eq!(find(Os::Linux, "Edit"), Some(true));
        let mac = layout(Os::Mac);
        assert!(mac[0]
            .entries
            .contains(&Entry::Predefined(Predefined::About)));
        assert!(mac[0]
            .entries
            .contains(&Entry::Predefined(Predefined::Quit)));
        assert!(!layout(Os::Windows)[0]
            .entries
            .contains(&Entry::Predefined(Predefined::Hide)));
    }

    #[test]
    fn accelerators_use_cmd_or_ctrl_and_match_the_table() {
        assert_eq!(
            accelerator("Mod+Shift+Y").as_deref(),
            Some("CmdOrCtrl+Shift+Y")
        );
        assert_eq!(accelerator("Mod+Alt+[").as_deref(), Some("CmdOrCtrl+Alt+["));
        assert_eq!(accelerator("Mod+,").as_deref(), Some("CmdOrCtrl+,"));
        assert_eq!(accelerator("Delete"), None);
        assert_eq!(accelerator(""), None);
        // The key a menu accelerator listens for is exactly the table's keys.
        for s in table() {
            if let Some(a) = accelerator(&s.keys) {
                assert_eq!(a.replace("CmdOrCtrl", "Mod"), s.keys);
            }
        }
    }

    #[test]
    fn every_accelerator_parses_and_none_repeats() {
        let mut seen = HashSet::new();
        for s in table() {
            let Some(a) = accelerator(&s.keys) else {
                continue;
            };
            let parsed: Result<muda::accelerator::Accelerator, _> = a.parse();
            assert!(parsed.is_ok(), "{} does not parse: {a}", s.action);
            assert!(seen.insert(a.clone()), "{a} is used twice");
        }
        for extra in ["CmdOrCtrl+Z", "CmdOrCtrl+Y", "CmdOrCtrl+Shift+Z"] {
            assert!(extra.parse::<muda::accelerator::Accelerator>().is_ok());
            assert!(!seen.contains(extra), "{extra} clashes with a shortcut");
        }
        // Edit's predefined keys are not reused for other actions.
        for taken in ["CmdOrCtrl+C", "CmdOrCtrl+X", "CmdOrCtrl+V", "CmdOrCtrl+A"] {
            assert!(!seen.contains(taken), "{taken} clashes with Edit");
        }
    }

    #[test]
    fn the_mac_import_is_only_in_the_mac_file_menu() {
        let file = |os| layout(os).into_iter().find(|m| m.title == "File").unwrap();
        assert!(file(Os::Mac)
            .entries
            .contains(&Entry::Action("importSwift")));
        assert!(!file(Os::Windows)
            .entries
            .contains(&Entry::Action("importSwift")));
        assert!(!file(Os::Linux)
            .entries
            .contains(&Entry::Action("importSwift")));
    }

    #[test]
    fn the_front_end_knows_every_action_in_the_table() {
        let ts = include_str!("../../src/shortcuts.ts");
        for s in table() {
            assert!(
                ts.contains(&format!("\"{}\"", s.action)),
                "{} is missing from the Action type",
                s.action
            );
        }
    }

    #[test]
    fn rows_grey_out_without_a_connection_or_table() {
        let none = MenuState::default();
        for a in [
            "disconnect",
            "refresh",
            "run",
            "rehearse",
            "runAtomically",
            "cancel",
            "newRow",
            "deleteRows",
            "setNull",
            "copyRows",
            "copySelection",
            "searchRows",
            "previousPage",
            "nextPage",
            "filterTables",
            "saveChanges",
            "revertChanges",
            "freezeFirstColumn",
        ] {
            assert!(!enabled(a, &none), "{a} should be off with nothing open");
        }
        for a in [
            "newConnection",
            "newWindow",
            "openFile",
            "closeWindow",
            "settings",
            "toggleSidebar",
            "toggleSQLPanel",
            "toggleTheme",
            "perfHud",
            "showShortcuts",
            "importSwift",
        ] {
            assert!(enabled(a, &none), "{a} should always work");
        }
        let table_open = MenuState {
            connected: true,
            has_table: true,
            can_insert: true,
            can_edit: true,
            has_result: true,
            ..Default::default()
        };
        assert!(
            enabled("run", &table_open)
                && enabled("newRow", &table_open)
                && enabled("copyRows", &table_open)
        );
        assert!(!enabled("deleteRows", &table_open), "needs a selection");
        assert!(!enabled("cancel", &table_open), "needs a running query");
        let busy = MenuState {
            running: true,
            ..table_open.clone()
        };
        assert!(enabled("cancel", &busy) && !enabled("run", &busy));
        let selected = MenuState {
            has_selection: true,
            ..table_open
        };
        assert!(
            enabled("deleteRows", &selected)
                && enabled("setNull", &selected)
                && enabled("copySelection", &selected)
        );
        let read_only = MenuState {
            can_edit: false,
            ..selected
        };
        assert!(!enabled("deleteRows", &read_only));
    }
}
