// A pretend backend for working on the UI in a plain browser (`npm run dev`),
// where there is no Tauri: it answers every command in src-tauri/src/commands.rs
// with sample data kept in memory. It is loaded only outside Tauri (see api.ts).
// The sample data is English, like the README snapshots of the macOS app.

import type {
  Cell,
  Column,
  ColumnSort,
  ConnectInfo,
  ConnectionDraft,
  ConnectionFolder,
  Engine,
  QueryResult,
  Relation,
  RowKey,
  SavedConnection,
  SchemaGraph,
  StoreSnapshot,
  Summary,
} from "./api";
import { historyHandlers } from "./history/mock";

import { bigSchema, demoLinks } from "./schema/sample";

// MARK: - Sample databases

interface Table {
  relation: Relation;
  columns: Column[];
  rows: Cell[][];
  /** Column names of the primary key; empty for views. */
  key: string[];
  /** A generated table too big to keep as arrays (`rows` is empty then). */
  big?: BigTable;
}

/** A million generated rows, made when read: ids in table order, edited rows kept aside.
 *  Enough to scroll the grid through page_block like a real table. */
class BigTable {
  /** Live ids in table order; null while it is still 1…size. */
  private ids: number[] | null = null;
  private edited = new Map<number, Cell[]>();
  private next: number;
  private views = new Map<string, number[]>();

  constructor(
    private size: number,
    private make: (id: number) => Cell[],
  ) {
    this.next = size + 1;
  }

  get length(): number {
    return this.ids ? this.ids.length : this.size;
  }

  row(id: number): Cell[] {
    return this.edited.get(id) ?? this.make(id);
  }

  private idAt(i: number): number {
    return this.ids ? this.ids[i] : i + 1;
  }

  /** The rows for a search and sort, as a function from position to id. */
  view(columns: Column[], search: string, sort: ColumnSort | null | undefined): { length: number; id: (i: number) => number } {
    const needle = search.trim().toLowerCase();
    const byId = !sort || sort.column === columns[0].name;
    if (!needle && byId) {
      const n = this.length;
      return { length: n, id: sort && !sort.ascending ? (i) => this.idAt(n - 1 - i) : (i) => this.idAt(i) };
    }
    const cacheKey = `${needle}\u0000${sort?.column ?? ""}\u0000${sort?.ascending ?? ""}`;
    let ids = this.views.get(cacheKey);
    if (!ids) {
      ids = [];
      for (let i = 0; i < this.length; i++) {
        const id = this.idAt(i);
        if (!needle || this.row(id).some((c) => c?.toLowerCase().includes(needle))) ids.push(id);
      }
      const c = sort ? columns.findIndex((x) => x.name === sort.column) : -1;
      if (sort && c > 0) {
        const values = new Map(ids.map((id) => [id, this.row(id)[c]]));
        const numeric = /int|numeric|real|double/.test(columns[c].type);
        ids.sort((a, b) => {
          const x = values.get(a) ?? null;
          const y = values.get(b) ?? null;
          if (x === y) return 0;
          if (x === null) return 1;
          if (y === null) return -1;
          const order = numeric ? Number(x) - Number(y) : x < y ? -1 : 1;
          return sort.ascending ? order : -order;
        });
      } else if (sort && !sort.ascending) {
        ids.reverse();
      }
      this.views.set(cacheKey, ids);
    }
    const list = ids;
    return { length: list.length, id: (i) => list[i] };
  }

  has(id: number): boolean {
    return this.ids ? this.ids.includes(id) : id >= 1 && id <= this.size;
  }

  update(id: number, row: Cell[]) {
    this.edited.set(id, row);
    this.views.clear();
  }

  insert(row: Cell[]): number {
    const id = this.next++;
    this.ids ??= Array.from({ length: this.size }, (_, i) => i + 1);
    this.ids.push(id);
    this.edited.set(id, [String(id), ...row.slice(1)]);
    this.views.clear();
    return id;
  }

  remove(id: number) {
    this.ids ??= Array.from({ length: this.size }, (_, i) => i + 1);
    const at = this.ids.indexOf(id);
    if (at >= 0) this.ids.splice(at, 1);
    this.edited.delete(id);
    this.views.clear();
  }
}

const kinds = ["page_view", "click", "signup", "purchase", "refund", "logout"];
const agents = ["Firefox/131", "Chrome/130", "Safari/18", "Edge/130"];
const lorem =
  "This note is long on purpose, so the grid has to cut it: it goes on about the order, the customer, the shipping address, the coupon, " +
  "the second shipping address, the time the parcel was late, and the reply from support, which was polite and also rather long. ";

/** One row of public.events, the same every time for the same id. */
function eventRow(id: number): Cell[] {
  // About half a minute apart, over a year.
  const at = new Date(Date.UTC(2024, 0, 1) + id * 31_557)
    .toISOString()
    .replace("T", " ")
    .slice(0, 19);
  return [
    String(id),
    String(((id * 7919) % 50_000) + 1),
    kinds[id % kinds.length],
    `{"ip": "10.${(id >> 16) & 255}.${(id >> 8) & 255}.${id & 255}", "agent": "${agents[id % agents.length]}"}`,
    ((id * 104_729) % 100_000 / 100).toFixed(2),
    `${at}+00`,
    id % 11 === 0 ? null : id % 97 === 0 ? `${lorem}${lorem}` : `${kinds[(id >> 3) % kinds.length]} #${id % 1000}`,
  ];
}

function eventsTable(): Table {
  const col = (name: string, type: string): Column => ({ name, type });
  return {
    relation: relation("public", "events", "table", 1_000_000),
    columns: [
      col("id", "bigint"),
      col("user_id", "integer"),
      col("kind", "text"),
      col("payload", "jsonb"),
      col("amount", "numeric(10,2)"),
      col("created_at", "timestamp with time zone"),
      col("note", "text"),
    ],
    rows: [],
    key: ["id"],
    big: new BigTable(1_000_000, eventRow),
  };
}

const first = ["Ada", "Grace", "Linus", "Margaret", "Ken", "Barbara", "Dennis", "Frances", "Alan", "Radia"];
const last = ["Lovelace", "Hopper", "Torvalds", "Hamilton", "Thompson", "Liskov", "Ritchie", "Allen", "Turing", "Perlman"];
const countries = ["Portugal", "United States", "Germany", "Japan", "Canada", "Brazil"];
const tiers = ["free", "pro", "team"];
const statuses = ["pending", "paid", "shipped", "refunded"];

function stamp(i: number): string {
  const day = new Date(Date.UTC(2025, 9, 1) + ((i * 7919) % 400) * 86400000);
  return `${day.toISOString().slice(0, 10)} 15:01:01.622618-03`;
}

function relation(schema: string, name: string, kind: Relation["kind"], estimate: number, subtype = ""): Relation {
  return { schema, name, kind, estimate, subtype };
}

function postgresDemo(): Table[] {
  const customers: Cell[][] = [];
  for (let i = 1; i <= 480; i++) {
    const name = `${first[(i - 1) % 10]} ${last[Math.floor((i - 1) / 10) % 10]}`;
    customers.push([
      String(i),
      name,
      `${name.toLowerCase().replace(" ", ".")}@example.com`,
      i % 17 === 0 ? null : countries[(i * 7) % countries.length],
      stamp(i),
      i % 7 === 0 ? "f" : "t",
      `{"tier": "${tiers[i % 3]}", "seats": ${(i % 9) + 1}}`,
    ]);
  }
  const addresses: Cell[][] = [];
  for (let i = 1; i <= 320; i++) {
    addresses.push([String(i), String(((i * 13) % 480) + 1), `${(i * 37) % 900 + 1} Main Street`, countries[i % 6], i % 5 === 0 ? null : String(10000 + i * 31)]);
  }
  const products: Cell[][] = [];
  const things = ["Keyboard", "Mouse", "Monitor", "Cable", "Dock", "Webcam", "Headset", "Stand"];
  for (let i = 1; i <= 40; i++) {
    products.push([String(i), `${things[i % 8]} ${Math.ceil(i / 8)}`, ((i * 1931) % 40000 / 100 + 9).toFixed(2), String((i * 17) % 120)]);
  }
  const orders: Cell[][] = [];
  for (let i = 1; i <= 2400; i++) {
    orders.push([String(i), String(((i * 7) % 480) + 1), statuses[i % 4], ((i * 7919) % 90000 / 100 + 10).toFixed(2), stamp(i + 3)]);
  }
  const items: Cell[][] = [];
  for (let i = 1; i <= 5200; i++) {
    items.push([String(i), String(((i * 3) % 2400) + 1), String((i % 40) + 1), String((i % 4) + 1)]);
  }
  const coupons: Cell[][] = [];
  for (let i = 1; i <= 12; i++) {
    coupons.push([`SAVE${i * 5}`, String(i * 5), i % 4 === 0 ? null : stamp(i * 11)]);
  }
  const revenue = statuses.map((s) => {
    const rows = orders.filter((o) => o[2] === s);
    return [s, String(rows.length), rows.reduce((a, o) => a + Number(o[3]), 0).toFixed(2)];
  });
  const col = (name: string, type: string): Column => ({ name, type });
  return [
    eventsTable(),
    {
      relation: relation("public", "addresses", "table", 320),
      columns: [col("id", "integer"), col("customer_id", "integer"), col("street", "text"), col("country", "text"), col("postcode", "text")],
      rows: addresses,
      key: ["id"],
    },
    {
      relation: relation("public", "customers", "table", 480),
      columns: [
        col("id", "integer"),
        col("name", "text"),
        col("email", "text"),
        col("country", "text"),
        col("signed_up", "timestamp with time zone"),
        col("active", "boolean"),
        col("plan", "jsonb"),
      ],
      rows: customers,
      key: ["id"],
    },
    {
      relation: relation("sales", "coupons", "table", 12),
      columns: [col("code", "text"), col("percent", "integer"), col("expires", "timestamp with time zone")],
      rows: coupons,
      key: ["code"],
    },
    {
      relation: relation("sales", "order_items", "table", 5200),
      columns: [col("id", "bigint"), col("order_id", "bigint"), col("product_id", "integer"), col("quantity", "integer")],
      rows: items,
      key: ["id"],
    },
    {
      relation: relation("sales", "orders", "table", 2400),
      columns: [col("id", "bigint"), col("customer_id", "integer"), col("status", "text"), col("total", "numeric(10,2)"), col("created_at", "timestamp with time zone")],
      rows: orders,
      key: ["id"],
    },
    {
      relation: relation("sales", "products", "table", 40),
      columns: [col("id", "integer"), col("name", "text"), col("price", "numeric(10,2)"), col("stock", "integer")],
      rows: products,
      key: ["id"],
    },
    {
      relation: relation("sales", "revenue_by_status", "view", -1),
      columns: [col("status", "text"), col("orders", "bigint"), col("revenue", "numeric")],
      rows: revenue,
      key: [],
    },
  ];
}

function redisDemo(): Table[] {
  const tables: Table[] = [];
  const add = (name: string, subtype: string, columns: string[], rows: Cell[][]) => {
    const group = name.includes(":") ? name.split(":")[0] : "";
    tables.push({
      relation: relation(group, name, "key", rows.length, subtype),
      columns: columns.map((c) => ({ name: c, type: "" })),
      rows,
      key: [],
    });
  };
  for (let i = 1; i <= 6; i++) {
    add(`user:${i}`, "hash", ["field", "value"], [
      ["name", `${first[i]} ${last[i]}`],
      ["email", `${first[i].toLowerCase()}@example.com`],
      ["visits", String(i * 13)],
    ]);
  }
  for (let i = 1; i <= 3; i++) add(`session:${"abc"[i - 1]}${i}f9`, "string", ["value"], [[`{"user": ${i}, "ttl": 3600}`]]);
  add("queue:emails", "list", ["index", "value"], [["0", "welcome:1"], ["1", "receipt:42"], ["2", "reset:7"]]);
  add("leaderboard", "zset", ["member", "score"], [["grace", "980"], ["linus", "870"], ["ada", "860"]]);
  add("feature-flags", "set", ["member"], [["dark-mode"], ["new-grid"], ["ssh"]]);
  return tables;
}

// MARK: - Saved connections

const now = new Date().toISOString();
const folders: ConnectionFolder[] = [{ id: "f-work", name: "Work", expanded: true }];
const saved: Array<SavedConnection & { url: string }> = [
  conn("c-analytics", "analytics @ server", "postgres", "postgres://report:secret@analytics.internal:5432/analytics", "", 0),
  conn("c-demo", "demo @ localhost", "postgres", "postgres://demo:demo@localhost:5432/demo", "", 1),
  conn("c-cache", "cache @ localhost", "redis", "redis://localhost:6379/0", "", 2),
  conn("c-prod", "production @ db.example.com", "postgres", "postgres://app:hunter2@db.example.com:5432/production?sslmode=require", "f-work", 0),
  conn("c-shop", "shop @ mysql.example.com", "mysql", "mysql://shop:pw@mysql.example.com:3306/shop", "f-work", 1),
];

function conn(id: string, name: string, engine: Engine, url: string, folderId: string, sortIndex: number) {
  return { id, name, engine, url, createdAt: now, lastUsed: null, folderId, sortIndex, ssh: null };
}

let agentsManageConnections = false;

function snapshot(): StoreSnapshot {
  return {
    connections: saved.map(({ url: _url, ...c }) => ({ ...c })),
    folders: folders.map((f) => ({ ...f })),
    agentsManageConnections,
  };
}

// MARK: - URLs

const MASK = "•••";

function clean(text: string): string {
  let s = text.trim();
  const env = s.match(/^[A-Z_][A-Z0-9_]*\s*=\s*(.*)$/);
  if (env) s = env[1].trim();
  return s.replace(/^["']|["']$/g, "");
}

function detect(text: string): Engine | null {
  const s = clean(text).toLowerCase();
  if (/^postgres(ql)?:\/\//.test(s) || /^(host|dbname)=/.test(s)) return "postgres";
  if (/^(mysql|mariadb):\/\//.test(s)) return "mysql";
  if (/^(sqlite3?|file):/.test(s) || /\.(db|sqlite3?)$/.test(s)) return "sqlite";
  if (/^(rediss?|valkeys?):\/\//.test(s)) return "redis";
  if (/^mongodb(\+srv)?:\/\//.test(s)) return "mongodb";
  return null;
}

function summarize(text: string): Summary | null {
  const engine = detect(text);
  if (!engine) return null;
  const s = clean(text);
  if (engine === "sqlite") {
    const path = s.replace(/^(sqlite3?|file):(\/\/)?/i, "");
    return { engine, database: path, host: null, port: null, user: null, tls: null, suggestedName: path.split(/[\\/]/).pop() ?? path };
  }
  const m = s.match(/^[a-z+]+:\/\/(?:([^:@/]*)(?::([^@/]*))?@)?([^:/?]*)(?::(\d+))?(?:\/([^?]*))?(?:\?(.*))?$/i);
  if (!m) return null;
  const [, user, , host, port, path, query] = m;
  const params = new URLSearchParams(query ?? "");
  let database = decodeURIComponent(path ?? "");
  if (engine === "redis") database = `db ${database || "0"}`;
  if (!database) database = user ? decodeURIComponent(user) : engine === "postgres" ? "postgres" : "";
  const tls = params.get("sslmode") ?? (s.startsWith("rediss") ? "on" : null);
  return {
    engine,
    database,
    host: host || "localhost",
    port: port ? Number(port) : null,
    user: user ? decodeURIComponent(user) : null,
    tls,
    suggestedName: `${database || engine} @ ${host || "localhost"}`,
  };
}

function mask(text: string): string {
  return text.replace(/^([a-z+]+:\/\/[^:@/]*:)([^@/]+)@/i, `$1${MASK}@`);
}

// MARK: - Sessions

interface MockSession {
  info: ConnectInfo;
  tables: Table[];
}

const sessions = new Map<string, MockSession>();
let nextSession = 1;

function session(args: Record<string, unknown>): MockSession {
  const s = sessions.get(String(args.sessionId));
  if (!s) throw "No open connection.";
  return s;
}

function table(s: MockSession, r: Relation): Table {
  const t = s.tables.find((x) => x.relation.schema === r.schema && x.relation.name === r.name);
  if (!t) throw `relation "${r.schema}.${r.name}" does not exist`;
  return t;
}

function filtered(t: Table, search: string, sort: ColumnSort | null | undefined): Cell[][] {
  const needle = search.trim().toLowerCase();
  let rows = needle ? t.rows.filter((r) => r.some((c) => c?.toLowerCase().includes(needle))) : t.rows;
  if (sort) {
    const i = t.columns.findIndex((c) => c.name === sort.column);
    if (i >= 0) {
      const numeric = rows.every((r) => r[i] === null || !Number.isNaN(Number(r[i])));
      rows = [...rows].sort((a, b) => {
        const x = a[i];
        const y = b[i];
        if (x === y) return 0;
        if (x === null) return 1;
        if (y === null) return -1;
        const order = numeric ? Number(x) - Number(y) : x.localeCompare(y);
        return sort.ascending ? order : -order;
      });
    }
  }
  return rows;
}

function result(columns: Column[], rows: Cell[][], command: string, extra: Partial<QueryResult> = {}): QueryResult {
  return { columns, rows, command, truncated: false, totalRows: rows.length, keys: null, note: null, ...extra };
}

/** A tiny SQL reader: enough for `select … from t [where …] [group by c] [order by …] [limit n]`. */
function runStatement(s: MockSession, statement: string): QueryResult {
  const sql = statement.trim().replace(/;$/, "");
  const verb = sql.split(/\s+/)[0]?.toUpperCase() ?? "";
  if (s.info.engine === "redis") return redisCommand(s, sql);
  if (verb !== "SELECT") {
    if (/^(BEGIN|COMMIT|ROLLBACK)$/i.test(verb)) return result([], [], verb);
    const target = sql.match(/^(?:update|delete\s+from|insert\s+into)\s+([\w."]+)/i)?.[1];
    if (target && !findByName(s, target)) throw `relation "${target}" does not exist`;
    return result([], [], verb === "INSERT" ? "INSERT 0 1" : `${verb} ${verb === "CREATE" ? "TABLE" : "0"}`);
  }
  const from = sql.match(/\bfrom\s+([\w."]+)(?:\s+(?:as\s+)?(\w+))?/i);
  if (!from) {
    const values = sql.replace(/^select\s+/i, "").split(",").map((v) => v.trim());
    return result(values.map((v, i) => ({ name: v.match(/\bas\s+(\w+)$/i)?.[1] ?? `?column?${i ? i : ""}`, type: "integer" })), [values.map((v) => v.replace(/\s+as\s+\w+$/i, ""))], "SELECT 1");
  }
  const t = findByName(s, from[1]);
  if (!t) throw `relation "${from[1]}" does not exist`;
  if (t.big) return bigSelect(t, sql);
  const alias = from[2] ? `${from[2]}.` : "";
  const strip = (x: string) => x.trim().replace(alias, "").replace(/"/g, "");
  const group = sql.match(/\bgroup\s+by\s+([\w.]+)/i)?.[1];
  const limit = Number(sql.match(/\blimit\s+(\d+)/i)?.[1] ?? 5000);
  const list = sql.slice(6, from.index).split(",").map(strip);
  if (group) {
    const g = t.columns.findIndex((c) => c.name === strip(group));
    const buckets = new Map<string, Cell[][]>();
    for (const r of t.rows) buckets.set(String(r[g]), [...(buckets.get(String(r[g])) ?? []), r]);
    const cols: Column[] = list.map((item) => ({ name: item.match(/\bas\s+(\w+)$/i)?.[1] ?? item, type: /count|sum/i.test(item) ? "numeric" : "text" }));
    let rows: Cell[][] = [...buckets].map(([k, rs]) =>
      list.map((item) => {
        const sum = item.match(/sum\(([\w.]+)\)/i);
        if (/count\(/i.test(item)) return String(rs.length);
        if (sum) {
          const i = t.columns.findIndex((c) => c.name === strip(sum[1]));
          return rs.reduce((a, r) => a + Number(r[i] ?? 0), 0).toFixed(2);
        }
        return k;
      }),
    );
    const order = sql.match(/\border\s+by\s+(\w+)(\s+desc)?/i);
    if (order) {
      const i = cols.findIndex((c) => c.name === order[1]);
      if (i >= 0) rows = rows.sort((a, b) => (Number(a[i]) - Number(b[i])) * (order[2] ? -1 : 1));
    }
    return result(cols, rows, `SELECT ${rows.length}`);
  }
  const indexes = list[0] === "*" ? t.columns.map((_, i) => i) : list.map((n) => t.columns.findIndex((c) => c.name === n));
  const missing = list.find((n, i) => n !== "*" && indexes[i] < 0);
  if (missing) throw `column "${missing}" does not exist`;
  const all = t.rows.map((r) => indexes.map((i) => r[i]));
  const rows = all.slice(0, Math.min(limit, 5000));
  return result(indexes.map((i) => t.columns[i]), rows, `SELECT ${rows.length}`, {
    truncated: all.length > 5000 && limit > 5000,
    totalRows: Math.min(all.length, limit),
  });
}

/** `select … from events [limit n]`: the first rows, like the drivers' 5,000-row cut. */
function bigSelect(t: Table, sql: string): QueryResult {
  const big = t.big!;
  const limit = Number(sql.match(/\blimit\s+(\d+)/i)?.[1] ?? big.length);
  const kept = Math.min(limit, 5000, big.length);
  const view = big.view(t.columns, "", null);
  const rows = Array.from({ length: kept }, (_, i) => big.row(view.id(i)));
  const total = Math.min(limit, big.length);
  return result(t.columns, rows, `SELECT ${total}`, { truncated: total > kept, totalRows: total });
}

function redisCommand(s: MockSession, text: string): QueryResult {
  const [cmd = "", ...args] = text.split(/\s+/);
  const t = s.tables.find((x) => x.relation.name === args[0]);
  switch (cmd.toUpperCase()) {
    case "PING":
      return result([{ name: "reply", type: "" }], [["PONG"]], "PING");
    case "KEYS":
    case "SCAN":
      return result([{ name: "key", type: "" }], s.tables.map((x) => [x.relation.name]), cmd.toUpperCase());
    case "GET":
    case "HGETALL":
    case "LRANGE":
    case "SMEMBERS":
    case "ZRANGE":
      return t ? result(t.columns, t.rows, cmd.toUpperCase()) : result([{ name: "reply", type: "" }], [[null]], cmd.toUpperCase());
    case "SET":
      return result([{ name: "reply", type: "" }], [["OK"]], "SET");
    default:
      throw `ERR unknown command '${cmd}'`;
  }
}

function findByName(s: MockSession, name: string): Table | undefined {
  const parts = name.replace(/"/g, "").split(".");
  const [schema, bare] = parts.length > 1 ? parts : [null, parts[0]];
  return s.tables.find((t) => t.relation.name === bare && (schema === null || t.relation.schema === schema));
}

function statements(text: string): string[] {
  return text.split(/;\s*(?:\n|$)/).map((x) => x.trim()).filter(Boolean);
}

/** Row-major, like the JSON the backend used to send; api.ts turns it into blocks. */
interface Execution {
  results: QueryResult[];
  durationMs: number;
}

function execution(s: MockSession, text: string): Execution {
  const started = performance.now();
  const results = statements(text).map((st) => runStatement(s, st));
  return { results, durationMs: Math.max(1, Math.round(performance.now() - started)) };
}

function keyIndex(t: Table, key: RowKey): number {
  const cols = key.columns.map((c) => t.columns.findIndex((x) => x.name === c));
  return t.rows.findIndex((r) => cols.every((ci, k) => r[ci] === key.values[k]));
}

function quote(name: string): string {
  return /^[a-z_][a-z0-9_]*$/.test(name) ? name : `"${name.replace(/"/g, '""')}"`;
}

function literal(value: Cell): string {
  return value === null ? "NULL" : `'${value.replace(/'/g, "''")}'`;
}

const pause = (ms: number) => new Promise((r) => setTimeout(r, ms));

// MARK: - Commands

const handlers: Record<string, (args: Record<string, unknown>) => unknown> = {
  detect_engine: (a) => detect(String(a.text)),
  summarize_url: (a) => summarize(String(a.text)),
  mask_url: (a) => mask(String(a.text)),
  check_for_updates: () => ({ available: false, version: null, notes: null }),
  install_update: () => null,

  connect: async (a) => {
    await pause(250);
    const stored = a.savedId ? saved.find((c) => c.id === a.savedId) : undefined;
    // With both, a.url is an edit of the saved connection being tried.
    const url = a.url ? String(a.url) : stored?.url ?? "";
    const summary = summarize(url);
    if (!summary) throw "This doesn't look like a database URL.";
    if (/example\.com|internal/.test(url)) {
      throw `Couldn't reach ${summary.host}: no server answers at port ${summary.port ?? 5432}.\nconnection refused`;
    }
    const sessionId = `s${nextSession++}`;
    const info: ConnectInfo = {
      sessionId,
      engine: summary.engine,
      serverVersion: summary.engine === "redis" ? "7.4.1" : summary.engine === "mysql" ? "11.4.2-MariaDB" : "16.4",
      name: stored?.name ?? summary.suggestedName,
      savedId: stored?.id ?? null,
      sshLabel: stored?.ssh ? `${stored.ssh.user ? stored.ssh.user + "@" : ""}${stored.ssh.host}` : null,
    };
    if (stored) stored.lastUsed = new Date().toISOString();
    sessions.set(sessionId, { info, tables: summary.engine === "redis" ? redisDemo() : postgresDemo() });
    return info;
  },
  disconnect: (a) => void sessions.delete(String(a.sessionId)),
  cancel: () => undefined,
  transaction_open: () => false,
  page_block: async (a) => {
    const t = table(session(a), a.relation as Relation);
    if (!t.big) return handlers.page(a);
    // About what a local server and the IPC take for a block.
    await pause(4 + Math.random() * 6);
    const view = t.big.view(t.columns, String(a.search ?? ""), a.sort as ColumnSort | null);
    const offset = Number(a.offset);
    const end = Math.min(view.length, offset + Number(a.limit));
    const rows: Cell[][] = [];
    for (let i = offset; i < end; i++) rows.push(t.big.row(view.id(i)));
    return result(t.columns, rows, `SELECT ${rows.length}`, {
      keys: { columns: t.key, kind: "primaryKey", values: rows.map((r) => [r[0]]) },
    });
  },
  relations: (a) => session(a).tables.map((t) => ({ ...t.relation })),
  page: async (a) => {
    const s = session(a);
    const t = table(s, a.relation as Relation);
    if (t.big) return handlers.page_block(a);
    await pause(40);
    const rows = filtered(t, String(a.search ?? ""), a.sort as ColumnSort | null);
    const offset = Number(a.offset);
    const pageRows = rows.slice(offset, offset + Number(a.limit));
    const keyCols = t.key.map((k) => t.columns.findIndex((c) => c.name === k));
    return result(t.columns, pageRows, `SELECT ${pageRows.length}`, {
      keys: t.key.length ? { columns: t.key, kind: "primaryKey", values: pageRows.map((r) => keyCols.map((i) => r[i])) } : null,
      note: t.relation.kind === "key" ? `${t.relation.subtype} · TTL none` : null,
    });
  },
  count: async (a) => {
    const s = session(a);
    const t = table(s, a.relation as Relation);
    if (t.big) {
      // Counting a big table takes a moment: the grid shows the estimate meanwhile.
      await pause(700);
      return t.big.view(t.columns, String(a.search ?? ""), null).length;
    }
    return filtered(t, String(a.search ?? ""), null).length;
  },
  execute: async (a) => {
    await pause(60);
    return execution(session(a), String(a.sql));
  },
  read_only: (a) => execution(session(a), String(a.text)),
  rehearse: async (a) => {
    await pause(60);
    const s = session(a);
    if (s.info.engine === "redis") throw "Redis can't try a statement and undo it; run it for real or not at all.";
    return execution(s, String(a.text));
  },
  execute_atomically: async (a) => {
    await pause(60);
    const s = session(a);
    if (s.info.engine === "redis" || s.info.engine === "mysql") {
      return { kind: "notAtomic", message: "MySQL commits each CREATE, ALTER, and DROP on its own, so this script can't run all or nothing." };
    }
    return { kind: "ran", execution: execution(s, String(a.text)) };
  },
  update_row: (a) => {
    const s = session(a);
    const r = a.relation as Relation;
    const t = table(s, r);
    const key = a.key as RowKey;
    const changes = a.changes as Array<{ column: string; value: Cell }>;
    if (t.big) {
      const id = Number(key.values[0]);
      if (!t.big.has(id)) throw "The row is gone: someone changed or deleted it. Reload the table.";
      const row = [...t.big.row(id)];
      for (const c of changes) row[t.columns.findIndex((x) => x.name === c.column)] = c.value;
      t.big.update(id, row);
    } else {
      const index = keyIndex(t, key);
      if (index < 0) throw "The row is gone: someone changed or deleted it. Reload the table.";
      const row = [...t.rows[index]];
      for (const c of changes) row[t.columns.findIndex((x) => x.name === c.column)] = c.value;
      t.rows[index] = row;
    }
    const set = changes.map((c) => `${quote(c.column)} = ${literal(c.value)}`).join(", ");
    const where = key.columns.map((c, i) => `${quote(c)} = ${literal(key.values[i])}`).join(" AND ");
    return { sql: `UPDATE ${quote(r.schema)}.${quote(r.name)} SET ${set} WHERE ${where}`, command: "UPDATE 1", affected: 1 };
  },
  insert_row: (a) => {
    const s = session(a);
    const r = a.relation as Relation;
    const t = table(s, r);
    const values = a.values as Array<{ column: string; value: Cell }>;
    const row: Cell[] = t.columns.map((c) => values.find((v) => v.column === c.name)?.value ?? null);
    if (t.big) t.big.insert(row);
    else {
      if (t.key.length === 1 && row[0] === null) row[0] = String(t.rows.length + 1);
      t.rows.push(row);
    }
    const cols = values.map((v) => quote(v.column)).join(", ");
    const vals = values.map((v) => literal(v.value)).join(", ");
    return { sql: `INSERT INTO ${quote(r.schema)}.${quote(r.name)} (${cols}) VALUES (${vals})`, command: "INSERT 0 1", affected: 1 };
  },
  delete_row: (a) => {
    const s = session(a);
    const r = a.relation as Relation;
    const t = table(s, r);
    const key = a.key as RowKey;
    if (t.big) {
      const id = Number(key.values[0]);
      if (!t.big.has(id)) throw "The row is gone: someone changed or deleted it. Reload the table.";
      t.big.remove(id);
    } else {
      const index = keyIndex(t, key);
      if (index < 0) throw "The row is gone: someone changed or deleted it. Reload the table.";
      t.rows.splice(index, 1);
    }
    const where = key.columns.map((c, i) => `${quote(c)} = ${literal(key.values[i])}`).join(" AND ");
    return { sql: `DELETE FROM ${quote(r.schema)}.${quote(r.name)} WHERE ${where}`, command: "DELETE 1", affected: 1 };
  },
  schema_graph: (a): SchemaGraph => {
    if (session(a).info.engine === "redis") return { tables: [], links: [] };
    // `?schemaTables=200`: a generated schema, to try the board with many tables.
    const many = Number(new URLSearchParams(location.search).get("schemaTables"));
    if (many > 0) return bigSchema(many);
    const tables = session(a).tables.map((t) => ({
      schema: t.relation.schema,
      name: t.relation.name,
      isView: t.relation.kind === "view",
      columns: t.columns.map((c) => ({ name: c.name, type: c.type, isPrimaryKey: t.key.includes(c.name), isNullable: !t.key.includes(c.name) })),
    }));
    return { tables, links: demoLinks(tables) };
  },
  qualified_name: (a) => {
    const r = a.relation as Relation;
    return session(a).info.engine === "redis" ? r.name : `${quote(r.schema)}.${quote(r.name)}`;
  },
  export_result: (a) => {
    const r = a.result as QueryResult;
    if (a.format === "json") {
      return JSON.stringify(r.rows.map((row) => Object.fromEntries(r.columns.map((c, i) => [c.name, row[i]]))), null, 2);
    }
    const esc = (v: Cell) => (v === null ? "" : /[",\n]/.test(v) ? `"${v.replace(/"/g, '""')}"` : v);
    return [r.columns.map((c) => esc(c.name)).join(","), ...r.rows.map((row) => row.map(esc).join(","))].join("\n");
  },

  list_connections: () => snapshot(),
  import_swift_connections: () => {
    throw "Importing connections from dbLens for Mac works only on macOS.";
  },
  save_connection: (a) => {
    const draft = a.draft as ConnectionDraft;
    const summary = summarize(draft.url);
    if (!summary) throw "This doesn't look like a database URL.";
    const existing = draft.id ? saved.find((c) => c.id === draft.id) : undefined;
    const url = existing && draft.url.includes(MASK) ? draft.url.replace(MASK, existing.url.match(/:\/\/[^:@/]*:([^@/]+)@/)?.[1] ?? "") : draft.url;
    const name = draft.name?.trim() || summary.suggestedName;
    if (existing) {
      Object.assign(existing, { name, url, engine: summary.engine, ssh: draft.ssh ?? null, folderId: draft.folderId ?? existing.folderId });
      const { url: _u, ...rest } = existing;
      return { ...rest };
    }
    const created = conn(`c-${Date.now()}`, name, summary.engine, url, draft.folderId ?? "", saved.length);
    created.ssh = (draft.ssh ?? null) as null;
    saved.push(created);
    const { url: _u, ...rest } = created;
    return { ...rest };
  },
  delete_connection: (a) => {
    const i = saved.findIndex((c) => c.id === a.id);
    if (i >= 0) saved.splice(i, 1);
  },
  rename_connection: (a) => {
    const c = saved.find((x) => x.id === a.id);
    if (!c) throw "No such connection.";
    c.name = String(a.name);
    const { url: _u, ...rest } = c;
    return { ...rest };
  },
  set_agents_can_write: (a) => {
    const c = saved.find((x) => x.id === a.id);
    if (!c) throw "No such connection.";
    c.agentsCanWrite = !!a.allowed;
    const { url: _u, ...rest } = c;
    return { ...rest };
  },
  set_agents_manage_connections: (a) => {
    agentsManageConnections = !!a.allowed;
  },
  connection_url: (a) => mask(saved.find((c) => c.id === a.id)?.url ?? ""),
  move_connection: (a) => {
    const c = saved.find((x) => x.id === a.id);
    if (c) c.folderId = String(a.folderId ?? "");
  },
  add_folder: (a) => {
    const f = { id: `f-${Date.now()}`, name: String(a.name), expanded: true };
    folders.push(f);
    return { ...f };
  },
  rename_folder: (a) => {
    const f = folders.find((x) => x.id === a.id);
    if (f) f.name = String(a.name);
  },
  delete_folder: (a) => {
    const i = folders.findIndex((x) => x.id === a.id);
    if (i >= 0) folders.splice(i, 1);
    for (const c of saved) if (c.folderId === a.id) c.folderId = "";
  },
  set_folder_expanded: (a) => {
    const f = folders.find((x) => x.id === a.id);
    if (f) f.expanded = Boolean(a.expanded);
  },
  ...historyHandlers,
};

/** Answers a command like Tauri's invoke: resolves with the value, rejects with a string. */
export async function mockInvoke<T>(command: string, args: Record<string, unknown>): Promise<T> {
  const handler = handlers[command];
  if (!handler) throw `Unknown command ${command}`;
  try {
    return (await handler(args)) as T;
  } catch (error) {
    throw typeof error === "string" ? error : String(error);
  }
}
