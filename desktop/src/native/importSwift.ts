// The saved connections of the Swift app (discontinued) come over, with their
// folders, SQL history, agent settings and app settings. macOS only; the Rust
// side is src-tauri/src/import_swift.rs. It runs once by itself at the first
// start (`autoImportFromMac`), and again, merging without duplicates, from
// File > Import Connections from dbLens for Mac….

import { api, errorText, inTauri, type ImportReport } from "../api";
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
    const parts = [t("Imported %@ connections, %@ folders and %@ history entries from dbLens for Mac.", report.imported, report.folders, report.history)];
    if (report.skipped > 0) parts.push(t("%@ were already here.", report.skipped));
    if (report.errors.length > 0) parts.push(t("Not imported: %@", report.errors.join("; ")));
    model.showToast(parts.join(" "));
  } catch (e) {
    model.showToast(errorText(e));
  }
}

/** First start after moving over from the Mac app: imports without asking and says so once. */
export async function autoImportFromMac(model: AppModel) {
  if (!canImportFromMac) return;
  let report: ImportReport | null;
  try {
    report = await api.autoImportSwift();
  } catch (e) {
    model.showToast(errorText(e));
    return;
  }
  if (!report) return;
  await model.loadStore();
  const parts: string[] = [];
  if (report.imported > 0) parts.push(t("Brought %@ connections from dbLens for Mac", report.imported));
  if (report.errors.length > 0) parts.push(t("Not imported: %@", report.errors.join("; ")));
  if (parts.length > 0) model.showToast(parts.join(". "));
}
