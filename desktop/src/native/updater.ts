// Updates (src-tauri/src/updater.rs): Help > Check for Updates (the menu row
// and its key), and the notice that a newer dbLens is out. The notice does not
// block: it sits in the corner, and "Later" hides it until the next check.
// Outside Tauri (plain browser) the backend is the mock, which finds nothing.

import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api, inTauri, type UpdateCheck } from "../api";
import { t } from "../i18n";
import type { AppModel } from "../model";

/** The event the backend sends to every window when an update is found (payload: UpdateCheck). */
const AVAILABLE_EVENT = "update-available";

/** The menu row and its key (Check for Updates…): asks now and says what it found. */
export async function checkForUpdatesNow(model: AppModel): Promise<void> {
  try {
    const result = await api.checkForUpdates();
    if (!result.available) model.showToast(t("dbLens is up to date."));
    // An available update shows in the notice (the backend sent the event to every window).
  } catch (error) {
    model.showToast(String(error));
  }
}

/** The notice's state: the one update shown (null: none), installing and the last error. */
export function useUpdateNotice() {
  const [update, setUpdate] = useState<UpdateCheck | null>(null);
  const [installing, setInstalling] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!inTauri) return;
    let gone = false;
    let unlisten: (() => void) | undefined;
    void listen<UpdateCheck>(AVAILABLE_EVENT, (event) => {
      if (!event.payload.available) return;
      setUpdate(event.payload);
      setError(null);
    }).then((stop) => {
      if (gone) stop();
      else unlisten = stop;
    });
    return () => {
      gone = true;
      unlisten?.();
    };
  }, []);

  /** Hides the notice until the next check finds an update. */
  const later = () => setUpdate(null);
  /** On success the app closes and relaunches, so this window never sees the reply. */
  const install = () => {
    setInstalling(true);
    setError(null);
    api.installUpdate().catch((reason: unknown) => {
      setError(String(reason));
      setInstalling(false);
    });
  };
  return { update, installing, error, later, install };
}
