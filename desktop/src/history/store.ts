// SQL history for the open connection: recording a run, and putting a past
// statement back in the editor (Use) or running it again (Run). The list
// itself lives in the Rust side (src-tauri/src/history.rs), per saved
// connection. A connection that was only pasted, never saved, has none.

import { api, errorText, type HistoryEntry } from "../api";
import type { AppModel } from "../model";

/** Asks the SQL panel to open its History popover (Mod+Shift+H, the menu). */
export const OPEN_HISTORY_EVENT = "dblens:open-history";

/** Called after every run in the SQL panel. Never throws: history is a nicety. */
export function recordRun(model: AppModel, sql: string, failed: boolean, savedId?: string | null) {
  const id = savedId ?? model.session?.savedId;
  if (!id || !sql.trim()) return;
  void api.addSqlHistory(id, sql, failed).catch(() => undefined);
}

export async function loadHistory(model: AppModel): Promise<HistoryEntry[]> {
  const id = model.session?.savedId;
  if (!id) return [];
  return api.sqlHistory(id);
}

export async function clearHistory(model: AppModel): Promise<void> {
  const id = model.session?.savedId;
  if (!id) return;
  try {
    await api.clearSqlHistory(id);
  } catch (e) {
    model.showToast(errorText(e));
  }
}

/** From the list: into the editor, and run it when asked. */
export async function useEntry(model: AppModel, entry: HistoryEntry, run: boolean) {
  model.setSQL(entry.sql);
  model.sqlSelection = "";
  model.changed();
  if (run) await model.run("plain", entry.sql);
}
