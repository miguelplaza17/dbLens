// Every action's keyboard shortcut, in one table. The window's key handler
// (App.tsx) and the button tooltips read it; the native menu (a later step)
// will be built from it too. "Mod" is Ctrl on Windows and Linux, Cmd on macOS.

import { t } from "./i18n";

export type Action =
  | "newConnection"
  | "disconnect"
  | "refresh"
  | "toggleSidebar"
  | "toggleSQLPanel"
  | "filterTables"
  | "searchRows"
  | "previousPage"
  | "nextPage"
  | "newRow"
  | "deleteRows"
  | "copyRows"
  | "run"
  | "rehearse"
  | "runAtomically"
  | "cancel"
  | "toggleTheme";

export interface Shortcut {
  action: Action;
  /** Like "Mod+Shift+N"; keys are KeyboardEvent.key values, letters upper case. */
  keys: string;
  /** English source text; show it through t(). */
  title: string;
  /** True when the key works while typing in a text field or the editor. */
  inFields?: boolean;
}

export const shortcuts: Shortcut[] = [
  { action: "newConnection", keys: "Mod+N", title: "New Connection…", inFields: true },
  { action: "disconnect", keys: "Mod+Shift+W", title: "Disconnect", inFields: true },
  { action: "refresh", keys: "Mod+R", title: "Reload", inFields: true },
  { action: "toggleSidebar", keys: "Mod+B", title: "Show or Hide the Sidebar", inFields: true },
  { action: "toggleSQLPanel", keys: "Mod+Shift+Y", title: "Show or Hide the SQL Panel", inFields: true },
  { action: "filterTables", keys: "Mod+Shift+F", title: "Filter Tables", inFields: true },
  { action: "searchRows", keys: "Mod+F", title: "Search rows", inFields: true },
  { action: "previousPage", keys: "Mod+Alt+[", title: "Previous page", inFields: true },
  { action: "nextPage", keys: "Mod+Alt+]", title: "Next page", inFields: true },
  { action: "newRow", keys: "Mod+Shift+N", title: "New Row", inFields: true },
  { action: "deleteRows", keys: "Delete", title: "Delete Row…" },
  { action: "copyRows", keys: "Mod+Shift+C", title: "Copy All", inFields: true },
  { action: "run", keys: "Mod+Enter", title: "Run", inFields: true },
  { action: "rehearse", keys: "Mod+Alt+Enter", title: "Rehearse", inFields: true },
  { action: "runAtomically", keys: "Mod+Shift+Enter", title: "Run All or Nothing", inFields: true },
  { action: "cancel", keys: "Mod+.", title: "Cancel", inFields: true },
  { action: "toggleTheme", keys: "Mod+Shift+L", title: "Light or Dark", inFields: true },
];

export const isMac = typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform);

/** The keys as the person sees them: "Ctrl+Shift+N", or "⇧⌘N" on a Mac. */
export function keyLabel(action: Action): string {
  const shortcut = shortcuts.find((s) => s.action === action);
  if (!shortcut) return "";
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
  return `${text} (${keyLabel(action)})`;
}

/** The action a key press is for, or null. `typing` is true in a text field or the editor. */
export function actionFor(event: KeyboardEvent, typing: boolean): Action | null {
  const mod = isMac ? event.metaKey : event.ctrlKey;
  for (const s of shortcuts) {
    if (typing && !s.inFields) continue;
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
  return shortcuts.map((s) => ({ title: t(s.title), keys: keyLabel(s.action) }));
}
