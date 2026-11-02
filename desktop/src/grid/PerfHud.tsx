// The perf HUD over the grid (Ctrl/Cmd+Shift+P): what perf.ts measures, read
// four times a second. The numbers are data attributes too, for scripted checks.

import { useEffect, useState, useSyncExternalStore } from "react";
import { formatNumber, t } from "../i18n";
import { hudVisible, resetPerf, snapshot, subscribeHud, type PerfSnapshot } from "./perf";

const ms = (v: number | null) => (v === null ? "–" : `${v < 10 ? v.toFixed(1) : Math.round(v)} ms`);

export function PerfHud() {
  const visible = useSyncExternalStore(subscribeHud, hudVisible);
  const [stats, setStats] = useState<PerfSnapshot>(() => snapshot());

  useEffect(() => {
    if (!visible) return;
    setStats(snapshot());
    const timer = setInterval(() => setStats(snapshot()), 250);
    return () => clearInterval(timer);
  }, [visible]);

  if (!visible) return null;
  const rows: Array<[string, string, string]> = [
    [t("First rows"), ms(stats.firstRowsMs), "first-rows"],
    [t("Block fetch p50"), ms(stats.p50), "p50"],
    [t("Block fetch p95"), ms(stats.p95), "p95"],
    [t("Fetches"), formatNumber(stats.fetches), "fetches"],
    [t("Scroll fps"), stats.fps === null ? "–" : stats.fps.toFixed(0), "fps"],
    [t("Longest frame"), ms(stats.worstFrameMs), "worst-frame"],
    [t("JS heap"), stats.heapMB === null ? "–" : `${stats.heapMB.toFixed(1)} MB`, "heap"],
    [t("Rows cached"), `${formatNumber(stats.rowsCached)} (${formatNumber(stats.blocksCached)})`, "rows-cached"],
  ];
  return (
    <div
      className="perf-hud"
      role="status"
      aria-label={t("Performance")}
      data-first-rows={stats.firstRowsMs ?? ""}
      data-p50={stats.p50 ?? ""}
      data-p95={stats.p95 ?? ""}
      data-fps={stats.fps ?? ""}
      data-worst-frame={stats.worstFrameMs ?? ""}
      data-heap={stats.heapMB ?? ""}
      data-rows-cached={stats.rowsCached}
      data-fetches={stats.fetches}
    >
      {rows.map(([label, value, key]) => (
        <div className="perf-hud-row" key={key}>
          <span>{label}</span>
          <span className="perf-hud-value">{value}</span>
        </div>
      ))}
      <button type="button" className="perf-hud-reset" onClick={resetPerf}>
        {t("Reset")}
      </button>
    </div>
  );
}
