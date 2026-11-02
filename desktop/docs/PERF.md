# Performance

What the performance core does (see [CONTRACT.md](../CONTRACT.md), "Binary
results", "Streaming" and "Pools") and what it measures.

## The bench

```sh
cd desktop/src-tauri
DBLENS_TEST_URL=postgres://dblens:dblens@127.0.0.1:5432/dblens_test \
DBLENS_TEST_MYSQL_URL=mysql://dblens:dblens@127.0.0.1:3306/dblens_test \
cargo test --release --test bench -- --ignored bench --nocapture
```

`tests/bench.rs` makes `bench_rows` (1,000,000 rows: `id bigint primary key`,
`n int`, `t text`, `ts timestamp`, `num numeric(12,2)`, `flag bool`, `doc
jsonb`/`json`) in each database it has a URL for, measures, prints a Markdown
table, and drops the table unless `DBLENS_BENCH_KEEP=1` (a kept table with
1,000,000 rows is reused). Times are medians of 3 to 7 runs, through
`PooledSession` (what the app uses), with the sink of `execute_stream` counting
bytes instead of a webview.

## Results

2026-11-02, Linux container (shared CPU), servers on the same machine
(PostgreSQL 16, MariaDB 10.11), release build without LTO
(`CARGO_PROFILE_RELEASE_LTO=off`, codegen-units 16, to build faster; the real
release profile is a little faster still).

| Measure | Postgres | MySQL (MariaDB) |
|---|---|---|
| connect, one connection | 16 ms (SCRAM) | 1.2 ms |
| first block, 500 rows (`page_block` 0, with key and type lookups) | 1.9 ms | 1.7 ms |
| next block, 500 rows (keyset from the previous block) | 2.1 ms | 2.5 ms |
| block at 900,000, plain OFFSET (the old `page`) | 56–67 ms | 110–119 ms |
| block at 900,000, cold jump (key-only seek, then keyset) | 59 ms | 108–133 ms |
| block at 900,500 after the one at 900,000 (keyset) | 1.8–2.6 ms | 1.9–3.1 ms |
| `count` (exact, 1M rows) | 30 ms | 109 ms |
| `execute_stream` `select * … limit 100000`: first block (500 rows) | 1.2 ms | 1.7 ms |
| `execute_stream` `select * … limit 100000`: all 100k rows | 116 ms | 124 ms |
| bytes / messages for those 100k rows | 7.9 MB / 24 | 7.8 MB / 24 |
| `execute_stream` `select *` (1M rows seen, 100k kept): first block | 1.3 ms | 1.6 ms |
| `execute_stream` `select *` (1M rows seen, 100k kept): total | 1.2 s | 0.9 s |
| `execute` 5,000 rows, whole (non-streamed path) | 7.1 ms | 9.9 ms |
| 500-row block: JSON vs MessagePack bytes | 52.9 KB vs 38.0 KB (72%) | 52.9 KB vs 37.4 KB (71%) |
| 500-row block: encode time, JSON vs MessagePack | 0.13 ms vs 0.05 ms | 0.13 ms vs 0.05 ms |
| 5,000-row execution: JSON vs MessagePack bytes | 538 KB vs 407 KB | 505 KB vs 370 KB |

Decoding in JS (Node 22 / V8, `@msgpack/msgpack` 3.1, the same 7 columns):
500 rows decode in 0.39 ms from 37.9 KB of MessagePack vs 0.36 ms for
`JSON.parse` of 52.9 KB of the old JSON; 5,000 rows 4.6 ms vs 3.5 ms. So V8
decodes about as fast either way; the savings are on the Rust side (2.6× faster
encode, no JSON escaping), in bytes over the IPC (~28% fewer), and in the
column-major shape (no per-row arrays; the grid reads `data[col][row]`).
Not measured yet: decode in WebKitGTK (JSC) and WebView2.

## What the numbers say

- **Scrolling in order is flat.** Keyset blocks cost the same at row 500 and at
  row 900,500 (~2 ms) where OFFSET grows with depth (56–119 ms at 900k). The grid
  should fetch blocks in order around the viewport (and prefetch the next one);
  each block hands its last key to the next.
- **A cold jump is still O(depth).** Dragging the scrollbar to row 900,000 with
  no block nearby costs about what OFFSET did: the key-only seek is an
  index-only scan in Postgres (up to ~40% cheaper than OFFSET on wide rows,
  little on this table) and no cheaper in InnoDB (the primary key is the
  clustered index). After that one block, its neighbours are keyset again.
- **First rows of a SQL run in ~1–2 ms**, then 5,000-row blocks; 100k rows
  arrive in ~120 ms. The cap (100k rows kept per result) keeps memory bounded;
  past it the rows are only counted (1M rows counted in about a second).
- **Counting is the slow part on big tables** (30–110 ms for 1M). `count` already
  returns `null` above 500,000 estimated rows; it runs on a browse connection, so
  it never holds up a block or the SQL panel.
- **Connections don't wait for each other.** `tests/pool.rs` checks that a
  page and a count return while `select pg_sleep(3)` / `select sleep(3)` runs in
  the SQL panel.
