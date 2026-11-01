// Text the UI builds from results: the status line, durations, TSV for the clipboard.

import { mainResult, type Execution, type QueryResult } from "./api";
import { formatNumber, t } from "./i18n";

/** "12 ms" or "1.40 s". */
export function duration(ms: number): string {
  return ms < 1000 ? `${Math.round(ms)} ms` : `${(ms / 1000).toFixed(2)} s`;
}

/** "4 rows · 1 ms", "UPDATE 3 · 2 commands · 4 ms" (Models.swift summary). */
export function executionSummary(execution: Execution): string {
  const parts: string[] = [];
  const main = mainResult(execution);
  if (main) {
    if (main.columns.length > 0) {
      if (main.truncated) parts.push(t("%@ of %@ rows (truncated)", main.rows.length, main.totalRows));
      else parts.push(main.totalRows === 1 ? t("%@ row", main.totalRows) : t("%@ rows", main.totalRows));
    } else if (main.command) {
      parts.push(main.command);
    }
  }
  if (execution.results.length > 1) parts.push(t("%lld commands", execution.results.length));
  parts.push(duration(execution.durationMs));
  return parts.join(" · ");
}

/** Rows as tab-separated text for a spreadsheet; NULL is empty. `rows` undefined = all. */
export function tsv(result: QueryResult, rows?: number[], header = true): string {
  const picked = rows ? [...rows].sort((a, b) => a - b) : result.rows.map((_, i) => i);
  const clean = (v: string | null) => (v ?? "").replace(/[\t\r\n]+/g, " ");
  const lines = picked.filter((i) => i < result.rows.length).map((i) => result.rows[i].map(clean).join("\t"));
  if (header) lines.unshift(result.columns.map((c) => clean(c.name)).join("\t"));
  return lines.join("\n");
}

export { formatNumber };

/** Puts text on the clipboard; falls back to a hidden text area. */
export async function copyText(text: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    const area = document.createElement("textarea");
    area.value = text;
    area.style.position = "fixed";
    area.style.opacity = "0";
    document.body.appendChild(area);
    area.select();
    document.execCommand("copy");
    area.remove();
  }
}
