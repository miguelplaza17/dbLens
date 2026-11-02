// File > Import Connections from dbLens for Mac…: the saved connections of the
// Swift app (discontinued) come over, with their folders and SSH settings.
// macOS only; the Rust side is src-tauri/src/import_swift.rs.

import { api, errorText, inTauri } from "../api";
import { t } from "../i18n";
import type { AppModel } from "../model";
import { isMac } from "../shortcuts";

/** Whether this window can offer the import at all. */
export const canImportFromMac = inTauri && isMac;

export async function importFromMac(model: AppModel) {
  if (!canImportFromMac) return;
  try {
    const report = await api.importSwiftConnections();
    await model.loadStore();
    const parts = [t("Imported %@ connections from dbLens for Mac.", report.imported)];
    if (report.skipped > 0) parts.push(t("%@ were already here.", report.skipped));
    if (report.errors.length > 0) parts.push(t("Not imported: %@", report.errors.join("; ")));
    model.showToast(parts.join(" "));
  } catch (e) {
    model.showToast(errorText(e));
  }
}
