// Text the UI builds from results: the status line, durations, TSV for the clipboard.

import { cellText, mainResult, type Execution, type ResultBlock } from "./api";
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
      if (main.truncated) parts.push(t("%@ of %@ rows (truncated)", main.rowCount, main.totalRows));
      else parts.push(main.totalRows === 1 ? t("%@ row", main.totalRows) : t("%@ rows", main.totalRows));
    } else if (main.command) {
      parts.push(main.command);
    }
  }
  if (execution.results.length > 1) parts.push(t("%lld commands", execution.results.length));
  parts.push(duration(execution.durationMs));
  return parts.join(" · ");
}

/** One spreadsheet field: quoted (inner quotes doubled) when it holds a tab, newline or quote. */
export function tsvField(v: string | null | undefined): string {
  const text = v ?? "";
  return /[\t\r\n"]/.test(text) ? `"${text.replace(/"/g, '""')}"` : text;
}

/** Rows as tab-separated text for a spreadsheet; NULL is empty. `rows` undefined = all. */
export function tsv(result: ResultBlock, rows?: number[], header = true, cols?: number[]): string {
  const picked = rows ? [...rows].sort((a, b) => a - b) : Array.from({ length: result.rowCount }, (_, i) => i);
  const columns = cols ?? result.columns.map((_, c) => c);
  const lines = picked
    .filter((i) => i < result.rowCount)
    .map((i) => columns.map((c) => tsvField(cellText(result, i, c))).join("\t"));
  if (header) lines.unshift(columns.map((c) => tsvField(result.columns[c]?.name)).join("\t"));
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
