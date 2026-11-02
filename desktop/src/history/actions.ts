// The menu and key actions of this feature: History, Open in Terminal, Copy URL,
// Let Agents Write.
// Back and Forward are in ../nav.ts.

import type { AppModel } from "../model";
import type { Action } from "../shortcuts";
import { copyUrl, copyUrlWithPassword, openInTerminal, toggleAgentsWrite } from "./connectionActions";
import { OPEN_HISTORY_EVENT } from "./store";

/** Returns true when it took the action. */
export function performHistory(model: AppModel, action: Action): boolean {
  switch (action) {
    case "history":
      if (!model.session) return true;
      // The popover lives in the SQL panel: show the panel first.
      if (!model.showSQLPanel) model.toggleSQLPanel();
      requestAnimationFrame(() => window.dispatchEvent(new Event(OPEN_HISTORY_EVENT)));
      return true;
    case "openTerminal":
      void openInTerminal(model);
      return true;
    case "copyUrl":
      void copyUrl(model);
      return true;
    case "copyUrlWithPassword":
      copyUrlWithPassword(model);
      return true;
    case "toggleAgentsWrite":
      void toggleAgentsWrite(model);
      return true;
    default:
      return false;
  }
}
