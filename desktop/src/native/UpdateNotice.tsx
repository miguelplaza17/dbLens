// The corner notice for an available update (native/updater.ts holds the logic).

import { t } from "../i18n";
import { useUpdateNotice } from "./updater";
import "./updater.css";

export function UpdateNotice() {
  const { update, installing, error, later, install } = useUpdateNotice();
  if (!update) return null;

  return (
    <section className="update-notice" role="status" aria-live="polite">
      <div className="update-notice-text">
        <strong>{t("dbLens %@ is available", update.version ?? "")}</strong>
        {update.notes && <p className="update-notice-notes">{update.notes}</p>}
        {error && <p className="update-notice-error">{error}</p>}
      </div>
      <div className="update-notice-actions">
        <button type="button" className="button" disabled={installing} onClick={later}>
          {t("Later")}
        </button>
        <button type="button" className="button prominent" disabled={installing} onClick={install}>
          {installing ? t("Installing…") : t("Install and Restart")}
        </button>
      </div>
    </section>
  );
}
