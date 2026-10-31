#!/bin/zsh
# Drives `dbLens --mcp` through every tool against a throwaway Postgres
# (APP_SEED_URL or DBLENS_TEST_URL; it creates and drops one table) on a fresh
# store in /tmp. Prints PASS or FAIL per check; exit 0 when all pass.
set -u
setopt null_glob
url=${APP_SEED_URL:-${DBLENS_TEST_URL:-}}
[[ -n $url ]] || { echo "set DBLENS_TEST_URL to a throwaway Postgres"; exit 1; }
app=~/Applications/dbLens.app/Contents/MacOS/dbLens
call() {  # call <writable 0|1> <json-rpc lines...>: prints one reply per line
  local writable=$1; shift
  printf '%s\n' '{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}' "$@" |
    APP_STORE=/tmp/dbLens-mcp.store APP_SEED_URL=$url APP_AGENTS_WRITE=$writable $app --mcp
}
tool() { print -r -- "{\"jsonrpc\":\"2.0\",\"id\":$1,\"method\":\"tools/call\",\"params\":{\"name\":\"$2\",\"arguments\":$3}}" }
rm -f /tmp/dbLens-mcp.store*
out=$(call 1 \
  "$(tool 1 execute '{"connection":"test","statement":"create table if not exists dblens_mcp(id int primary key, nome text)"}')" \
  "$(tool 2 execute '{"connection":"test","statement":"insert into dblens_mcp values (1,'"'"'a'"'"'),(2,'"'"'b'"'"') on conflict do nothing"}')" \
  "$(tool 3 list_connections '{}')" \
  "$(tool 4 list_tables '{"connection":"test"}')" \
  "$(tool 5 describe_table '{"connection":"test","table":"dblens_mcp"}')" \
  "$(tool 6 query '{"connection":"test","query":"select * from dblens_mcp order by id","max_rows":1}')" \
  "$(tool 7 query '{"connection":"test","query":"delete from dblens_mcp"}')" \
  "$(tool 8 execute '{"connection":"test","statement":"delete from dblens_mcp","dry_run":true}')" \
  "$(tool 9 query '{"connection":"test","query":"select count(*) from dblens_mcp"}')" \
  "$(tool 13 describe_schema '{"connection":"test"}')" \
  "$(tool 14 list_extensions '{"connection":"test"}')" \
  "$(tool 15 install_extension '{"connection":"test","name":"pg_trgm"}')" \
  "$(tool 16 query '{"connection":"test","query":"select similarity('"'"'dblens'"'"', '"'"'dblenz'"'"') > 0"}')" \
  "$(tool 17 install_extension '{"connection":"test","name":"no_such_ext; drop table x"}')")
ro=$(call 0 "$(tool 10 execute '{"connection":"test","statement":"delete from dblens_mcp"}')" \
            "$(tool 19 install_extension '{"connection":"test","name":"unaccent"}')" \
            "$(tool 11 query '{"connection":"nope","query":"select 1"}')")
call 1 "$(tool 12 execute '{"connection":"test","statement":"drop table dblens_mcp"}')" \
       "$(tool 18 execute '{"connection":"test","statement":"drop extension if exists pg_trgm"}')" >/dev/null
rm -f /tmp/dbLens-mcp.store*
print -r -- "$out
$ro" | python3 -c '
import sys, json
replies = {m["id"]: m.get("result", {}) for m in map(json.loads, sys.stdin) if m.get("id")}
def text(i): return replies[i]["content"][0]["text"]
def err(i): return replies[i]["isError"]
checks = [
    ("execute writes when the connection allows it", not err(1) and not err(2)),
    ("list_connections names the connection, never the URL", "\"test\"" in text(3) and "://" not in text(3)),
    ("list_tables lists the table", "dblens_mcp" in text(4)),
    ("describe_table gives columns and the primary key", "\"primary_key\":[\"id\"]" in text(5)),
    ("query returns rows and says when it cut them", "\"rows\":[[\"1\",\"a\"]]" in text(6) and "truncated" in text(6)),
    ("query refuses a write", err(7)),
    ("execute dry_run reports and rolls back", "DELETE 2" in text(8) and not err(8)),
    ("nothing was deleted", "[[\"2\"]]" in text(9)),
    ("execute is refused where writing is off", err(10) and "Let Agents Write" in text(10)),
    ("an unknown connection is an error", err(11)),
    ("list_extensions shows installed and available ones", "\"plpgsql\"" in text(14) and "\"available\"" in text(14)),
    ("install_extension installs an available one", not err(15) and "pg_trgm" in text(15)),
    ("its functions work right after", "[[\"t\"]]" in text(16)),
    ("install_extension refuses names the server does not list", err(17)),
    ("install_extension is refused where writing is off", err(19)),
    ("describe_schema lists every table with its columns", "dblens_mcp (table" in text(13) and "id integer PK" in text(13)),
]
for name, ok in checks: print(("PASS " if ok else "FAIL ") + name)
sys.exit(0 if all(ok for _, ok in checks) else 1)'
