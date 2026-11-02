// The actions that came with the native menu. App.tsx handles the older ones;
// this handles New Window, Open SQLite File, Close Window and the rest, and
// returns true when it took the action.

import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { invoke } from "@tauri-apps/api/core";
import { inTauri } from "../api";
import { copyText, tsv } from "../format";
import { t } from "../i18n";
import type { AppModel } from "../model";
import type { Action } from "../shortcuts";
import { openDatabaseFile } from "./bridge";
import { SET_NULL_EVENT } from "../grid/commands";
import { importFromMac } from "./importSwift";
import { openSettings } from "../settings/store";
import { checkForUpdatesNow } from "./updater";

export function performNative(model: AppModel, action: Action): boolean {
  switch (action) {
    case "newWindow":
      if (inTauri) void invoke("new_window");
      return true;
    case "closeWindow":
      if (inTauri) void getCurrentWindow().close();
      return true;
    case "openFile":
      void pickDatabase(model);
      return true;
    case "settings":
      openSettings(t("Settings")).catch((error) => model.showToast(String(error)));
      return true;
    case "showShortcuts":
      model.open({ kind: "shortcuts" });
      return true;
    case "checkForUpdates":
      void checkForUpdatesNow(model);
      return true;
    case "importSwift":
      void importFromMac(model);
      return true;
    case "saveChanges":
    case "revertChanges":
    case "freezeFirstColumn":
      // The grid takes these (App.perform asks it first); with no grid there is nothing to do.
      return true;
    case "setNull":
      window.dispatchEvent(new CustomEvent(SET_NULL_EVENT));
      return true;
    case "copySelection": {
      const result = model.shownResult;
      const rows = [...model.selection];
      if (!result || rows.length === 0) return true;
      void copyText(tsv(result, rows)).then(() => model.showToast(t("Copied %@ rows", rows.length)));
      return true;
    }
    default:
      return false;
  }
}

async function pickDatabase(model: AppModel) {
  if (!inTauri) return;
  const picked = await openDialog({
    multiple: false,
    directory: false,
    filters: [{ name: "SQLite", extensions: ["db", "sqlite", "sqlite3", "db3"] }],
  });
  if (typeof picked === "string") await openDatabaseFile(model, picked);
}
