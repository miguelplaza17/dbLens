// Connects this window to the native shell: menu clicks, the menu's enabled
// state, dropped files and text, and files queued by a second launch.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { api, inTauri } from "../api";
import { t } from "../i18n";
import type { AppModel } from "../model";
import { shortcuts, type Action } from "../shortcuts";
import { dispatch } from "./actions";
import { connectionText, firstDatabase, sqliteUrl } from "./drop";
import { installCloseGuard } from "./closeGuard";

const known = new Set<string>(shortcuts.map((s) => s.action));

/** What the menu needs to know about this window (menu.rs MenuState). */
export function menuState(model: AppModel) {
  const result = model.shownResult;
  return {
    connected: !!model.session,
    hasTable: !!model.session && !!model.relation && model.shown === "table",
    canInsert: model.canInsertRows,
    canEdit: model.canEditRows,
    hasSelection: model.selection.size > 0,
    hasResult: !!result,
    // Previous and next page scroll the grid a screen at a time.
    canBack: !!result,
    canForward: !!result,
    sqlPanel: model.showSQLPanel,
    running: model.running,
  };
}

/** Connects to a SQLite file and saves it under the file's name. */
export async function openDatabaseFile(model: AppModel, path: string) {
  const name = path.split(/[\\/]/).pop() ?? path;
  try {
    await model.connectNew({ url: sqliteUrl(path), name, ssh: null, editing: null });
  } catch (e) {
    model.showToast(String(e));
  }
}

/** Undo and Redo from the menu, on the field or editor that has focus. */
function editCommand(kind: "undo" | "redo") {
  const target = document.activeElement as HTMLElement | null;
  if (target?.closest(".cm-editor")) {
    const mac = /Mac/.test(navigator.platform);
    const redoShift = kind === "redo" && mac;
    target.dispatchEvent(
      new KeyboardEvent("keydown", {
        key: kind === "undo" || redoShift ? "z" : "y",
        code: kind === "undo" || redoShift ? "KeyZ" : "KeyY",
        ctrlKey: !mac,
        metaKey: mac,
        shiftKey: redoShift,
        bubbles: true,
        cancelable: true,
      }),
    );
    return;
  }
  document.execCommand(kind);
}

/** Starts listening; returns the function that stops. Does nothing outside Tauri. */
export function installNative(model: AppModel): () => void {
  if (!inTauri) return () => undefined;
  const stops: Array<() => void> = [];
  let alive = true;
  const keep = (promise: Promise<() => void>) =>
    void promise.then((stop) => (alive ? stops.push(stop) : stop())).catch(() => undefined);

  // Unsaved grid edits are asked about before the window closes or the app quits.
  stops.push(installCloseGuard(model));

  keep(
    listen<string>("menu-action", (event) => {
      const id = event.payload;
      if (id === "undo" || id === "redo") editCommand(id);
      else if (known.has(id)) {
        // A sheet is open: only window-level actions work behind it, as with keys.
        if (model.sheet && !["newWindow", "closeWindow", "toggleTheme", "perfHud"].includes(id)) return;
        dispatch(id as Action);
      }
    }),
  );

  // The menu's wording in the UI language (the language is fixed at start).
  const labels: Record<string, string> = {
    undo: t("Undo"),
    redo: t("Redo"),
    quit: t(/Mac/.test(navigator.platform) ? "Quit dbLens" : "Quit"),
  };
  for (const s of shortcuts) labels[s.action] = t(s.menu ?? s.title);
  for (const title of ["File", "Edit", "View", "Query", "Window", "Help"]) labels[`menu:${title}`] = t(title);
  void invoke("set_menu_labels", { labels }).catch(() => undefined);

  // The menu is shared by all windows, so the focused one sets its state.
  let sent = "";
  const push = () => {
    if (!document.hasFocus()) return;
    const state = JSON.stringify(menuState(model));
    if (state === sent) return;
    sent = state;
    void invoke("set_menu_state", { state: JSON.parse(state) }).catch(() => undefined);
  };
  const unsubscribe = model.subscribe(push);
  const onFocus = () => {
    sent = "";
    push();
  };
  window.addEventListener("focus", onFocus);
  push();

  // Files dropped from the file manager.
  keep(
    getCurrentWebview().onDragDropEvent((event) => {
      if (event.payload.type !== "drop") return;
      const file = firstDatabase(event.payload.paths);
      if (file) void openDatabaseFile(model, file);
      else if (event.payload.paths.length > 0) model.showToast(t("Drop a .db, .sqlite or .sqlite3 file"));
    }),
  );

  // Text dropped on the window: a connection string opens the connect sheet.
  // Not on Windows: there Tauri's file-drop handler owns the web view and these
  // DOM events never fire (see dragConnections.ts); paste the string instead.
  const windows = /Win/.test(navigator.platform);
  const onDragOver = (event: DragEvent) => {
    if (event.dataTransfer?.types.includes("text/plain")) event.preventDefault();
  };
  const onDrop = async (event: DragEvent) => {
    const text = connectionText(event.dataTransfer?.getData("text/plain") ?? "");
    if (!text) return;
    event.preventDefault();
    if (await api.detectEngine(text).catch(() => null)) model.open({ kind: "connection", editing: null, url: text });
  };
  if (!windows) {
    window.addEventListener("dragover", onDragOver);
    window.addEventListener("drop", onDrop);
  }

  // Files queued for this window by a second launch.
  void invoke<string[]>("take_pending_open")
    .then((paths) => {
      const file = firstDatabase(paths);
      if (file) void openDatabaseFile(model, file);
    })
    .catch(() => undefined);

  return () => {
    alive = false;
    stops.forEach((stop) => stop());
    unsubscribe();
    window.removeEventListener("focus", onFocus);
    window.removeEventListener("dragover", onDragOver);
    window.removeEventListener("drop", onDrop);
  };
}
