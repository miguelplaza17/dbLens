// Every action's keyboard shortcut, in one table. The window's key handler
// (App.tsx) and the button tooltips read it; the native menu
// is built from it too (shortcuts.json). "Mod" is Ctrl on Windows and Linux, Cmd on macOS.

import { t } from "./i18n";
import table from "./shortcuts.json";

export type Action =
  | "newConnection"
  | "newWindow"
  | "openFile"
  | "importSwift"
  | "checkForUpdates"
  | "closeWindow"
  | "disconnect"
  | "settings"
  | "refresh"
  | "toggleSidebar"
  | "toggleSQLPanel"
  | "filterTables"
  | "searchRows"
  | "previousPage"
  | "nextPage"
  | "newRow"
  | "deleteRows"
  | "saveChanges"
  | "revertChanges"
  | "setNull"
  | "copySelection"
  | "copyRows"
  | "run"
  | "rehearse"
  | "runAtomically"
  | "cancel"
  | "freezeFirstColumn"
  | "toggleTheme"
  | "perfHud"
  | "showShortcuts"
  | "showTable"
  | "showQuery"
  | "showSchema"
  | "zoomIn"
  | "zoomOut"
  | "zoomToFit"
  | "history"
  | "openTerminal"
  | "copyUrl"
  | "copyUrlWithPassword"
  | "toggleAgentsWrite"
  | "goBack"
  | "goForward";

export interface Shortcut {
  action: Action;
  /** Like "Mod+Shift+N"; keys are KeyboardEvent.key values, letters upper case. Empty: menu only. */
  keys: string;
  /** English source text; show it through t(). */
  title: string;
  /** The native menu's wording when it differs from the title. */
  menu?: string;
  /** True when the key works while typing in a text field or the editor. */
  inFields?: boolean;
  /** Only on this OS (the menu leaves it out elsewhere). */
  os?: "mac";
}

// shortcuts.json is also compiled into the Rust side (src-tauri/src/menu.rs),
// which builds the native menu's accelerators from the same rows.
export const isMac = typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform);

export const shortcuts: Shortcut[] = (table as Shortcut[]).filter((s) => !s.os || (s.os === "mac" && isMac));

/** The keys as the person sees them: "Ctrl+Shift+N", or "⇧⌘N" on a Mac. */
export function keyLabel(action: Action): string {
  const shortcut = shortcuts.find((s) => s.action === action);
  if (!shortcut?.keys) return "";
  const parts = shortcut.keys.split("+");
  if (isMac) {
    const symbols: Record<string, string> = { Mod: "⌘", Shift: "⇧", Alt: "⌥", Enter: "↩", Delete: "⌫" };
    const mods = ["Alt", "Shift", "Mod"].filter((m) => parts.includes(m)).map((m) => symbols[m]);
    const key = parts.filter((p) => !["Mod", "Shift", "Alt"].includes(p)).map((p) => symbols[p] ?? p);
    return [...mods, ...key].join("");
  }
  const names: Record<string, string> = { Mod: "Ctrl", Enter: "Enter", Delete: "Del" };
  return parts.map((p) => names[p] ?? p).join("+");
}

/** "Run (Ctrl+Enter)": a tooltip with the shortcut. */
export function withKeys(text: string, action: Action): string {
  const label = keyLabel(action);
  return label ? `${text} (${label})` : text;
}

/** The action a key press is for, or null. `typing` is true in a text field or the editor. */
export function actionFor(event: KeyboardEvent, typing: boolean): Action | null {
  const mod = isMac ? event.metaKey : event.ctrlKey;
  for (const s of shortcuts) {
    if (!s.keys || (typing && !s.inFields)) continue;
    const parts = s.keys.split("+");
    const key = parts[parts.length - 1];
    if (parts.includes("Mod") !== mod) continue;
    if (parts.includes("Shift") !== event.shiftKey) continue;
    if (parts.includes("Alt") !== event.altKey) continue;
    if (sameKey(event, key)) return s.action;
  }
  return null;
}

function sameKey(event: KeyboardEvent, key: string): boolean {
  if (key === "[" || key === "]") return event.code === (key === "[" ? "BracketLeft" : "BracketRight");
  if (key === ".") return event.code === "Period" || event.key === ".";
  if (key === "Delete") return event.key === "Delete" || event.key === "Backspace";
  if (key.length === 1) return event.code === `Key${key}` || event.key.toUpperCase() === key;
  return event.key === key;
}

/** The table of shortcuts, translated, for a help sheet. */
export function shortcutList(): Array<{ title: string; keys: string }> {
  return shortcuts.filter((s) => s.keys).map((s) => ({ title: t(s.title), keys: keyLabel(s.action) }));
}
