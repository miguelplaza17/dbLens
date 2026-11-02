// Sample foreign keys for the mock backend (src/mock.ts), and a generated
// schema of many tables (`?schemaTables=200` in the dev URL) to check that
// the board stays smooth with a big database.

import type { SchemaGraph, SchemaLink, SchemaTable } from "../api";

/** The demo database's foreign keys (only those whose tables exist are kept). */
export function demoLinks(tables: SchemaTable[]): SchemaLink[] {
  const ids = new Set(tables.map((t) => `${t.schema}.${t.name}`));
  const fk = (from: string, fromColumn: string, to: string): SchemaLink => ({
    name: `${from.split(".")[1]}_${fromColumn}_fkey`,
    from,
    fromColumns: [fromColumn],
    to,
    toColumns: ["id"],
  });
  return [
    fk("public.addresses", "customer_id", "public.customers"),
    fk("public.events", "user_id", "public.customers"),
    fk("sales.orders", "customer_id", "public.customers"),
    fk("sales.order_items", "order_id", "sales.orders"),
    fk("sales.order_items", "product_id", "sales.products"),
  ].filter((l) => ids.has(l.from) && ids.has(l.to));
}

const schemas = ["app", "billing", "crm", "inventory", "audit"];
const nouns = ["account", "invoice", "contact", "item", "event", "shipment", "payment", "note", "tag", "region", "plan", "seat"];
const types = ["integer", "text", "timestamp with time zone", "boolean", "numeric(10,2)", "jsonb", "uuid", "bigint"];

/** `count` generated tables in five schemas; about two in three point at an earlier table. */
export function bigSchema(count: number): SchemaGraph {
  const tables: SchemaTable[] = [];
  const links: SchemaLink[] = [];
  for (let i = 0; i < count; i++) {
    const schema = schemas[i % schemas.length];
    const name = `${nouns[(i * 7) % nouns.length]}_${i}`;
    const columns = [{ name: "id", type: "bigint", isPrimaryKey: true, isNullable: false }];
    const width = 3 + ((i * 5) % 9);
    for (let c = 1; c < width; c++) {
      columns.push({ name: `${nouns[(i + c) % nouns.length]}_${c}`, type: types[(i + c) % types.length], isPrimaryKey: false, isNullable: c % 3 === 0 });
    }
    if (i > 0 && i % 3 !== 0) {
      const target = tables[(i * 37 + 11) % Math.min(i, 40)];
      const column = `${target.name}_id`;
      columns.splice(1, 0, { name: column, type: "bigint", isPrimaryKey: false, isNullable: false });
      links.push({ name: `${name}_${column}_fkey`, from: `${schema}.${name}`, fromColumns: [column], to: `${target.schema}.${target.name}`, toColumns: ["id"] });
    }
    tables.push({ schema, name, isView: i % 23 === 0, columns });
  }
  return { tables, links };
}
