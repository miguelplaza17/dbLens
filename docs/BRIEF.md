# dbLens

A fast, minimal database client for the Mac (Postgres first; MySQL, SQLite, Redis, and
MongoDB next, each detected from its URL): paste a connection string, click a table,
see and edit the rows, run SQL. For people who find pgAdmin, DBeaver, and the like
slow and crowded for everyday work.

## Main jobs
1. Connect by pasting any Postgres connection string (or filling in a form), directly or
   through SSH, and reopen saved connections with one click.
2. Browse schemas and tables; a click opens the rows at once, a page at a time.
3. Edit rows in place, add and delete rows, copy rows or the whole grid.
4. Write and run SQL; the result shows in the same grid, with a log of the last run.

## Data
- Connection: name, SSH settings, last used (SwiftData); its URL is encrypted apart,
  under a Secure Enclave key.
- Everything else (schemas, rows, results) comes from the server and is not stored.

## Shape
One window: sidebar (connections and tables), the grid, and a foldable SQL panel at the
bottom (editor and log). macOS 26 or later, dark only. No accounts, no telemetry.

## Not in scope
- Schema designers, ER diagrams, dashboards, charts.
- Query history and autocompletion (maybe later, if they stay out of the way).
