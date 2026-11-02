// The app settings (src-tauri/src/settings.rs): read, change, and follow them
// live. Rust keeps them in settings.json and sends every change to all windows
// (event "settings-changed"), so the theme and accent change everywhere at once.
// In a plain browser (vite dev) they live in localStorage and travel between
// tabs over a BroadcastChannel.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { inTauri } from "../api";
import { applyAccent, type AccentId } from "./accents";

export type Appearance = "system" | "light" | "dark";

export interface Settings {
  appearance: Appearance;
  accent: AccentId;
  /** Settings > Agents > Let agents add and remove connections. */
  agentsManageConnections: boolean;
  checkForUpdates: boolean;
  updateAutomatically: boolean;
}

export interface SettingsInfo {
  version: string;
  /** This program, for `<path> --mcp`. */
  executable: string;
  /** null: nothing on disk (DBLENS_STORE=memory). */
  dataFolder: string | null;
  os: "macos" | "windows" | "linux" | string;
}

export const SETTINGS_EVENT = "settings-changed";

const defaults: Settings = {
  appearance: "dark",
  accent: "system",
  agentsManageConnections: false,
  checkForUpdates: true,
  updateAutomatically: false,
};

// MARK: - Browser stand-in

const MOCK_KEY = "dblens.mockSettings";
const channel = !inTauri && typeof BroadcastChannel !== "undefined" ? new BroadcastChannel("dblens-settings") : null;
const localListeners = new Set<(s: Settings) => void>();

function mockRead(): Settings {
  try {
    const text = localStorage.getItem(MOCK_KEY);
    return text ? { ...defaults, ...(JSON.parse(text) as Partial<Settings>) } : { ...defaults };
  } catch {
    return { ...defaults };
  }
}

function mockWrite(next: Settings) {
  try {
    localStorage.setItem(MOCK_KEY, JSON.stringify(next));
  } catch {
    // Private window: the change still reaches this tab.
  }
  channel?.postMessage(next);
  for (const listener of localListeners) listener(next);
}

// MARK: - Commands

export function getSettings(): Promise<Settings> {
  if (!inTauri) return Promise.resolve(mockRead());
  return invoke<Settings>("get_settings");
}

/** Changes some settings; every window hears about it. */
export function setSettings(patch: Partial<Settings>): Promise<Settings> {
  if (!inTauri) {
    const next = { ...mockRead(), ...patch };
    mockWrite(next);
    return Promise.resolve(next);
  }
  return invoke<Settings>("set_settings", { patch });
}

export function settingsInfo(): Promise<SettingsInfo> {
  if (!inTauri) {
    return Promise.resolve({ version: "0.1.0", executable: "/usr/bin/dblens", dataFolder: "~/.local/share/dblens", os: "linux" });
  }
  return invoke<SettingsInfo>("settings_info");
}

/** Opens the Settings window, or brings it to the front. */
export function openSettings(title: string): Promise<void> {
  if (!inTauri) {
    window.open(`${location.pathname}?window=settings`, "dblens-settings", "width=720,height=520");
    return Promise.resolve();
  }
  return invoke<void>("open_settings", { title });
}

/** Sizes the Settings window to `height` CSS pixels of content. */
export function fitSettings(height: number): Promise<void> {
  if (!inTauri) return Promise.resolve();
  return invoke<void>("fit_settings", { height });
}

/** Opens the data folder in the file manager. */
export function showDataFolder(): Promise<void> {
  if (!inTauri) return Promise.resolve();
  return invoke<void>("show_data_folder");
}

/** Calls `listener` with every change, from any window. Returns the unsubscribe. */
export function onSettings(listener: (s: Settings) => void): () => void {
  if (!inTauri) {
    localListeners.add(listener);
    const onMessage = (event: MessageEvent<Settings>) => listener(event.data);
    channel?.addEventListener("message", onMessage);
    return () => {
      localListeners.delete(listener);
      channel?.removeEventListener("message", onMessage);
    };
  }
  let stop: (() => void) | null = null;
  let gone = false;
  void listen<Settings>(SETTINGS_EVENT, (event) => listener(event.payload)).then((unlisten) => {
    if (gone) unlisten();
    else stop = unlisten;
  });
  return () => {
    gone = true;
    stop?.();
  };
}

// MARK: - Applying them

const darkQuery = typeof matchMedia === "function" ? matchMedia("(prefers-color-scheme: dark)") : null;

/** "light" or "dark": the appearance, with "system" resolved from the OS. */
export function effectiveTheme(appearance: Appearance): "light" | "dark" {
  if (appearance === "system") return darkQuery?.matches === false ? "light" : "dark";
  return appearance;
}

/**
 * Follows the settings in this window: the accent at once, and the theme
 * through `setTheme` (the main window keeps it in its model; the Settings
 * window sets data-theme itself). A `?theme=` in the URL (screenshots) wins.
 * Returns the unsubscribe.
 */
export function followSettings(setTheme: (theme: "light" | "dark") => void, onChange?: (s: Settings) => void): () => void {
  const forced = new URLSearchParams(location.search).get("theme");
  let appearance: Appearance = "dark";
  const apply = (s: Settings) => {
    appearance = s.appearance;
    applyAccent(s.accent);
    if (!forced) setTheme(effectiveTheme(s.appearance));
    onChange?.(s);
  };
  let stopped = false;
  void getSettings().then((s) => {
    if (!stopped) apply(s);
  });
  const stop = onSettings(apply);
  const onSystem = () => {
    if (appearance === "system" && !forced) setTheme(effectiveTheme("system"));
  };
  darkQuery?.addEventListener("change", onSystem);
  return () => {
    stopped = true;
    stop();
    darkQuery?.removeEventListener("change", onSystem);
  };
}
