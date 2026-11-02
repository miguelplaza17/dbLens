// The History button of the SQL panel and its popover (SQLHistoryList in
// SQLPanel.swift): a filter, Clear, and the statements run on this connection,
// newest first, each with Use (into the editor) and Run.

import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { HistoryEntry } from "../api";
import { Empty } from "../components/Empty";
import { Icon } from "../components/Icon";
import { language, t } from "../i18n";
import { useModel } from "../model";
import { withKeys } from "../shortcuts";
import { clearHistory, loadHistory, OPEN_HISTORY_EVENT, useEntry } from "./store";
import "./history.css";

export function HistoryButton() {
  const model = useModel();
  const [open, setOpen] = useState(false);
  const button = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    const show = () => setOpen(true);
    window.addEventListener(OPEN_HISTORY_EVENT, show);
    return () => window.removeEventListener(OPEN_HISTORY_EVENT, show);
  }, []);

  // A disconnect or another connection closes it.
  const savedId = model.session?.savedId ?? null;
  useEffect(() => setOpen(false), [savedId]);

  return (
    <>
      <button
        ref={button}
        className={`icon-button${open ? " active" : ""}`}
        disabled={!model.session}
        onClick={() => setOpen((v) => !v)}
        aria-haspopup="dialog"
        aria-expanded={open}
        title={withKeys(t("History"), "history")}
        aria-label={t("History")}
      >
        <Icon name="history" size={14} />
      </button>
      {open && <Popover anchor={button} onClose={() => setOpen(false)} />}
    </>
  );
}

function Popover({ anchor, onClose }: { anchor: React.RefObject<HTMLButtonElement | null>; onClose: () => void }) {
  const model = useModel();
  const root = useRef<HTMLDivElement>(null);
  const [entries, setEntries] = useState<HistoryEntry[] | null>(null);
  const [filter, setFilter] = useState("");
  const [place, setPlace] = useState<{ right: number; bottom: number } | null>(null);
  const saved = !!model.session?.savedId;

  const reload = useCallback(() => {
    loadHistory(model).then(setEntries, () => setEntries([]));
  }, [model]);
  useEffect(reload, [reload]);

  // Above the button (the panel is at the bottom of the window), right edges together.
  useLayoutEffect(() => {
    const rect = anchor.current?.getBoundingClientRect();
    if (rect) setPlace({ right: Math.max(8, window.innerWidth - rect.right), bottom: window.innerHeight - rect.top + 4 });
  }, [anchor]);

  useEffect(() => {
    const away = (event: MouseEvent) => {
      const target = event.target as Node;
      if (!root.current?.contains(target) && !anchor.current?.contains(target)) onClose();
    };
    const key = (event: KeyboardEvent) => event.key === "Escape" && onClose();
    window.addEventListener("mousedown", away);
    window.addEventListener("keydown", key);
    return () => {
      window.removeEventListener("mousedown", away);
      window.removeEventListener("keydown", key);
    };
  }, [anchor, onClose]);

  const shown = useMemo(() => {
    const needle = filter.trim().toLowerCase();
    return (entries ?? []).filter((e) => !needle || e.sql.toLowerCase().includes(needle));
  }, [entries, filter]);

  const pick = (entry: HistoryEntry, run: boolean) => {
    onClose();
    void useEntry(model, entry, run);
  };
  const clear = async () => {
    await clearHistory(model);
    reload();
  };

  return (
    <div
      ref={root}
      className="history-popover"
      role="dialog"
      aria-label={t("History")}
      style={place ? { right: place.right, bottom: place.bottom } : { visibility: "hidden" }}
    >
      <div className="history-head">
        <input
          type="search"
          value={filter}
          autoFocus
          placeholder={t("Filter History")}
          onChange={(e) => setFilter(e.target.value)}
          spellCheck={false}
        />
        <button className="button" onClick={() => void clear()} disabled={!entries?.length}>
          {t("Clear")}
        </button>
      </div>
      {shown.length === 0 ? (
        <div className="history-empty">
          <Empty icon="history" title={!saved ? t("Save this connection to keep its history.") : entries?.length ? t("No statement matches.") : t("Nothing run on this connection yet.")} />
        </div>
      ) : (
        <ul className="history-list">
          {shown.map((entry, i) => (
            <li key={`${entry.date}-${i}`} className="history-row">
              <span className={`history-state${entry.failed ? " failed" : ""}`} title={entry.failed ? t("It failed") : t("It ran")}>
                <Icon name={entry.failed ? "xCircle" : "checkCircle"} size={12} />
              </span>
              <button className="history-sql" onClick={() => pick(entry, false)} title={entry.sql}>
                <span className="history-text">{firstLines(entry.sql)}</span>
                <span className="history-date tertiary">
                  {when(entry.date)}
                  {entry.viaAgent ? ` · ${t("Agent")}` : ""}
                </span>
              </button>
              <button className="button small" onClick={() => pick(entry, false)} title={t("Put it in the editor")}>
                {t("Use")}
              </button>
              <button
                className="button small"
                disabled={model.running}
                onClick={() => pick(entry, true)}
                title={t("Run it again")}
              >
                {t("Run")}
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

/** The statement on one or two lines for the list; the full text is the tooltip. */
function firstLines(sql: string): string {
  return sql.trim().split("\n").slice(0, 2).join("\n");
}

function when(iso: string): string {
  const date = new Date(iso);
  return Number.isNaN(date.getTime()) ? "" : date.toLocaleString(language, { dateStyle: "short", timeStyle: "short" });
}
