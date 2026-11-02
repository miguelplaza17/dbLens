// Answers the window's close requests (close button, Close Window, Quit; see
// src-tauri/src/close_guard.rs): with unsaved grid edits, asks Save, Don't
// Save or Cancel first. Pending edits live only here, so Rust can't decide.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { message } from "@tauri-apps/plugin-dialog";
import { t } from "../i18n";
import type { AppModel } from "../model";

const CLOSE_EVENT = "dblens-close-requested";

/** Starts answering; returns the function that stops. */
export function installCloseGuard(model: AppModel): () => void {
  let asking = false;
  const confirm = () => void invoke("close_confirmed").catch(() => undefined);
  const stop = listen(CLOSE_EVENT, async () => {
    if (asking) return;
    if (!model.hasUnsavedEdits) return confirm();
    asking = true;
    try {
      const save = t("Save");
      const discard = t("Don't Save");
      const choice = await message(t("Save the changes to this table before closing?"), {
        title: t("Unsaved changes"),
        kind: "warning",
        buttons: { yes: save, no: discard, cancel: t("Cancel") },
      });
      if (choice === save || choice === "Yes") {
        // A row that fails to save keeps the window open, with its error shown.
        if (await model.saveUnsavedEdits()) confirm();
      } else if (choice === discard || choice === "No") {
        confirm();
      }
    } catch {
      // No dialog: keep the window rather than lose the edits.
    } finally {
      asking = false;
    }
  });
  // Registered only once listening, so a request never goes unanswered.
  void stop.then(() => invoke("close_guard_ready")).catch(() => undefined);
  return () => void stop.then((unlisten) => unlisten()).catch(() => undefined);
}
