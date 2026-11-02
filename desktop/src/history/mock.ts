// The history commands for the browser-only mock backend (src/mock.ts).

import type { HistoryEntry } from "../api";

const lists = new Map<string, HistoryEntry[]>();

function seed(): HistoryEntry[] {
  const now = Date.now();
  const at = (minutes: number) => new Date(now - minutes * 60_000).toISOString();
  return [
    { sql: "select count(*) from orders where status = 'open';", failed: false, date: at(3), viaAgent: false },
    { sql: "update customers\n  set tier = 'gold'\n  where lifetime_value > 10000;", failed: false, date: at(41), viaAgent: false },
    { sql: "select * from orderz limit 10;", failed: true, date: at(95), viaAgent: false },
    { sql: "select now();", failed: false, date: at(300), viaAgent: true },
  ];
}

function list(id: string): HistoryEntry[] {
  if (!lists.has(id)) lists.set(id, new URLSearchParams(location.search).has("emptyHistory") ? [] : seed());
  return lists.get(id) as HistoryEntry[];
}

export const historyHandlers: Record<string, (args: Record<string, unknown>) => unknown> = {
  sql_history: (a) => [...list(String(a.connectionId))],
  add_sql_history: (a) => {
    const entries = list(String(a.connectionId));
    const entry = { sql: String(a.sql), failed: Boolean(a.failed), date: new Date().toISOString(), viaAgent: false };
    if (entries[0]?.sql === entry.sql) entries[0] = entry;
    else entries.unshift(entry);
    entries.length = Math.min(entries.length, 200);
  },
  clear_sql_history: (a) => {
    if (typeof a.connectionId === "string") lists.set(a.connectionId, []);
    else lists.clear();
  },
  open_in_terminal: () => ({ tool: "psql", missing: null, install: null }),
  connection_url_with_password: () => "postgres://ana:secret@localhost:5432/shop",
};
