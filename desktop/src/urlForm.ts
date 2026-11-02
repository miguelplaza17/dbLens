// The connection sheet's two ways to say where the database is: a URL, or
// fields. These turn one into the other (ConnectionSheet.swift switchMode,
// ConnectionURL.compose). The backend still reads and checks the URL itself.

import { MASK, type Engine } from "./api";

export interface UrlFields {
  engine: Engine;
  host: string;
  port: string;
  database: string;
  user: string;
  password: string;
  sslmode: string;
}

export const engines: Engine[] = ["postgres", "mysql", "sqlite", "redis", "mongodb"];

export const defaultPorts: Record<Engine, number | null> = {
  postgres: 5432,
  mysql: 3306,
  sqlite: null,
  redis: 6379,
  mongodb: 27017,
};

const schemes: Record<Engine, string> = {
  postgres: "postgres",
  mysql: "mysql",
  sqlite: "sqlite",
  redis: "redis",
  mongodb: "mongodb",
};

/** Postgres sslmode choices; "" is the driver's default. */
export const sslModes: Array<[string, string]> = [
  ["", "Default (prefer)"],
  ["disable", "Off"],
  ["require", "Require"],
  ["verify-ca", "Verify CA"],
  ["verify-full", "Verify CA and Host"],
];

const encode = (s: string) => encodeURIComponent(s).replace(/%E2%80%A2/g, "•");

/** The URL for the fields. A password still showing the mask stays the mask, so the saved one is kept. */
export function compose(f: UrlFields): string {
  if (f.engine === "sqlite") return `sqlite://${f.database.trim()}`;
  const user = f.user.trim();
  const auth = user || f.password ? `${encode(user)}${f.password ? `:${f.password === MASK ? MASK : encode(f.password)}` : ""}@` : "";
  const port = f.port.trim() ? `:${f.port.trim()}` : "";
  const database = f.database.trim() ? `/${encodeURIComponent(f.database.trim())}` : "";
  const query = f.engine === "postgres" && f.sslmode ? `?sslmode=${f.sslmode}` : "";
  return `${schemes[f.engine]}://${auth}${f.host.trim()}${port}${database}${query}`;
}

/** The fields of a URL, as far as they go; unknown parts are left empty. */
export function decompose(url: string, engine: Engine): UrlFields {
  const fields: UrlFields = { engine, host: "", port: "", database: "", user: "", password: "", sslmode: "" };
  const text = url.trim().replace(/^[A-Z_][A-Z0-9_]*\s*=\s*/, "").replace(/^["']|["']$/g, "");
  if (engine === "sqlite") {
    fields.database = text.replace(/^(sqlite3?|file):(\/\/)?/i, "");
    return fields;
  }
  const m = text.match(/^[a-z0-9+.-]+:\/\/(?:([^:@/]*)(?::([^@/]*))?@)?([^:/?,]*)(?::(\d+))?[^/?]*(?:\/([^?]*))?(?:\?(.*))?$/i);
  if (!m) return fields;
  const decode = (s: string | undefined) => {
    try {
      return decodeURIComponent(s ?? "");
    } catch {
      return s ?? "";
    }
  };
  fields.user = decode(m[1]);
  fields.password = decode(m[2]);
  fields.host = m[3] ?? "";
  fields.port = m[4] ?? "";
  fields.database = decode(m[5]);
  const ssl = new URLSearchParams(m[6] ?? "").get("sslmode") ?? "";
  fields.sslmode = sslModes.some(([v]) => v === ssl) ? ssl : "";
  return fields;
}
