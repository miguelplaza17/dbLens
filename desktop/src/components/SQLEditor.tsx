// The editor in the SQL panel: CodeMirror 6 with SQL highlighting for the
// connection's dialect and table names to complete. Redis and MongoDB get a
// plain editor (commands). Mod-Enter runs; the colors come from theme.css.

import { useEffect, useRef } from "react";
import {
  autocompletion,
  closeBrackets,
  closeBracketsKeymap,
  closeCompletion,
  completionKeymap,
  type Completion,
  type CompletionContext,
  type CompletionResult,
  type CompletionSource,
} from "@codemirror/autocomplete";
import { defaultKeymap, history, historyKeymap, indentWithTab } from "@codemirror/commands";
import { MySQL, PostgreSQL, SQLite, sql, type SQLConfig } from "@codemirror/lang-sql";
import { bracketMatching, HighlightStyle, indentOnInput, StreamLanguage, syntaxHighlighting, type StreamParser } from "@codemirror/language";
import { Compartment, EditorState, Prec } from "@codemirror/state";
import {
  drawSelection,
  EditorView,
  highlightActiveLine,
  highlightActiveLineGutter,
  keymap,
  lineNumbers,
  placeholder as placeholderText,
} from "@codemirror/view";
import { tags } from "@lezer/highlight";
import { api, type Engine, type Relation } from "../api";
import { model } from "../model";

const highlight = HighlightStyle.define([
  { tag: [tags.keyword, tags.operatorKeyword, tags.modifier], color: "var(--syntax-keyword)" },
  { tag: [tags.function(tags.variableName), tags.standard(tags.name)], color: "var(--syntax-function)" },
  { tag: [tags.string, tags.special(tags.string)], color: "var(--syntax-string)" },
  { tag: [tags.number, tags.bool, tags.null], color: "var(--syntax-number)" },
  { tag: [tags.lineComment, tags.blockComment, tags.comment], color: "var(--syntax-comment)", fontStyle: "italic" },
  { tag: [tags.typeName, tags.standard(tags.typeName), tags.atom], color: "var(--syntax-type)" },
  { tag: [tags.operator, tags.punctuation], color: "var(--syntax-operator)" },
]);

const theme = EditorView.theme({
  "&": { height: "100%", color: "var(--text)", backgroundColor: "transparent", fontSize: "var(--font-size-mono)" },
  "&.cm-focused": { outline: "none" },
  ".cm-scroller": { fontFamily: "var(--font-mono)", lineHeight: "1.55" },
  ".cm-content": { caretColor: "var(--accent)", padding: "var(--space-sm) 0" },
  ".cm-cursor, .cm-dropCursor": { borderLeftColor: "var(--accent)", borderLeftWidth: "2px" },
  "&.cm-focused > .cm-scroller > .cm-selectionLayer .cm-selectionBackground, .cm-selectionBackground, ::selection": {
    backgroundColor: "var(--accent-soft) !important",
  },
  ".cm-gutters": { backgroundColor: "transparent", color: "var(--text-tertiary)", border: "none" },
  ".cm-lineNumbers .cm-gutterElement": { padding: "0 var(--space-sm) 0 var(--space-md)", minWidth: "32px" },
  ".cm-activeLine": { backgroundColor: "var(--accent-faint)" },
  ".cm-activeLineGutter": { backgroundColor: "transparent", color: "var(--text-secondary)" },
  ".cm-matchingBracket": { backgroundColor: "var(--accent-soft)", outline: "none" },
  ".cm-placeholder": { color: "var(--text-tertiary)" },
  ".cm-tooltip": { backgroundColor: "var(--raised)", border: "none", borderRadius: "var(--radius-md)", boxShadow: "var(--shadow)", overflow: "hidden" },
  ".cm-tooltip-autocomplete > ul > li": { padding: "var(--space-xxs) var(--space-sm)", fontFamily: "var(--font-mono)" },
  ".cm-tooltip-autocomplete > ul > li[aria-selected]": { backgroundColor: "var(--accent)", color: "var(--on-accent)" },
  ".cm-completionDetail": { color: "var(--text-tertiary)", fontStyle: "normal", marginLeft: "var(--space-sm)" },
});

const REDIS_COMMANDS = (
  "APPEND AUTH BGSAVE BITCOUNT BITOP BLPOP BRPOP CLIENT CONFIG COPY DBSIZE DECR DECRBY DEL DUMP ECHO EVAL EXISTS EXPIRE " +
  "EXPIREAT FLUSHALL FLUSHDB GEO.ADD GEOADD GEODIST GEOPOS GEOSEARCH GET GETDEL GETEX GETRANGE GETSET HDEL HEXISTS HGET " +
  "HGETALL HINCRBY HINCRBYFLOAT HKEYS HLEN HMGET HMSET HSCAN HSET HSETNX HVALS INCR INCRBY INCRBYFLOAT INFO KEYS LASTSAVE " +
  "LINDEX LINSERT LLEN LPOP LPUSH LPUSHX LRANGE LREM LSET LTRIM MGET MONITOR MOVE MSET MSETNX OBJECT PERSIST PEXPIRE PING " +
  "PSETEX PTTL PUBLISH RANDOMKEY RENAME RENAMENX RPOP RPOPLPUSH RPUSH RPUSHX SADD SAVE SCAN SCARD SDIFF SDIFFSTORE SELECT " +
  "SET SETEX SETNX SETRANGE SINTER SINTERSTORE SISMEMBER SMEMBERS SMOVE SORT SPOP SRANDMEMBER SREM SSCAN STRLEN SUNION " +
  "SUNIONSTORE TIME TOUCH TTL TYPE UNLINK XADD XDEL XGROUP XLEN XRANGE XREAD XREVRANGE XTRIM ZADD ZCARD ZCOUNT ZINCRBY " +
  "ZPOPMAX ZPOPMIN ZRANGE ZRANGEBYSCORE ZRANK ZREM ZREVRANGE ZREVRANK ZSCAN ZSCORE"
).split(" ");

const MONGO_METHODS = (
  "aggregate bulkWrite countDocuments createIndex deleteMany deleteOne distinct drop dropIndex estimatedDocumentCount find " +
  "findOne findOneAndDelete findOneAndReplace findOneAndUpdate getIndexes insertMany insertOne renameCollection replaceOne " +
  "updateMany updateOne"
).split(" ");
const MONGO_DB_METHODS = "getCollectionNames getCollectionInfos stats runCommand createCollection dropDatabase".split(" ");
const MONGO_OPERATORS = (
  "$eq $ne $gt $gte $lt $lte $in $nin $and $or $not $nor $exists $type $regex $expr $all $elemMatch $size $set $unset $inc " +
  "$push $pull $addToSet $pop $match $group $project $sort $limit $skip $lookup $unwind $count $sum $avg $min $max"
).split(" ");
const MONGO_WORDS = new Set("db use show ObjectId ISODate Date NumberLong NumberInt NumberDecimal new true false null".split(" "));

interface CommandLineState {
  first: boolean;
}

function readString(stream: Parameters<StreamParser<unknown>["token"]>[0], quote: string, escapes: boolean) {
  while (!stream.eol()) {
    const ch = stream.next();
    if (ch === "\\" && escapes) stream.next();
    else if (ch === quote) break;
  }
  return "string";
}

/** Redis: the command word starting each line, quoted arguments, numbers, # comments. */
const redisParser: StreamParser<CommandLineState> = {
  startState: () => ({ first: true }),
  token(stream, state) {
    if (stream.sol()) state.first = true;
    if (stream.eatSpace()) return null;
    if (state.first) {
      state.first = false;
      if (stream.peek() === "#") {
        stream.skipToEnd();
        return "comment";
      }
      if (stream.match(/^[A-Za-z][A-Za-z.]*/)) return "keyword";
    }
    const ch = stream.peek();
    if (ch === '"' || ch === "'") {
      stream.next();
      return readString(stream, ch, ch === '"');
    }
    if (stream.match(/^-?\d+(?:\.\d+)?(?=\s|$)/)) return "number";
    stream.eatWhile(/\S/);
    return null;
  },
};

/** mongosh: db and method calls, $operators, strings, numbers, // comments. */
const mongoParser: StreamParser<null> = {
  startState: () => null,
  token(stream) {
    if (stream.eatSpace()) return null;
    if (stream.match("//")) {
      stream.skipToEnd();
      return "comment";
    }
    const ch = stream.peek();
    if (ch === '"' || ch === "'") {
      stream.next();
      return readString(stream, ch, true);
    }
    if (stream.match(/^\$[A-Za-z]+/)) return "typeName";
    if (stream.match(/^-?\d+(?:\.\d+)?/)) return "number";
    if (stream.match(/^[A-Za-z_][A-Za-z0-9_]*/)) {
      const word = stream.current();
      if (MONGO_WORDS.has(word)) return "keyword";
      if (stream.string[stream.start - 1] === "." && stream.match(/^\s*\(/, false)) return "variableName.function";
      return null;
    }
    stream.next();
    return null;
  },
};

const redisLanguage = StreamLanguage.define(redisParser);
const mongoLanguage = StreamLanguage.define(mongoParser);

// Columns by table, read once per connection (and again after a Reload, which
// hands the editor a new `relations`), the first time a completion needs them.
type ColumnMap = Map<string, string[]>;
let columnCache: { sessionId: string; relations: Relation[]; columns: Promise<ColumnMap> } | null = null;

function columnsFor(relations: Relation[]): Promise<ColumnMap> {
  const sessionId = model.session?.sessionId;
  if (!sessionId) return Promise.resolve(new Map());
  if (columnCache?.sessionId === sessionId && columnCache.relations === relations) return columnCache.columns;
  const columns = api
    .schemaGraph(sessionId)
    .then((graph) => {
      const map: ColumnMap = new Map();
      for (const table of graph.tables) {
        const names = table.columns.map((c) => c.name);
        map.set(table.name.toLowerCase(), names);
        map.set(`${table.schema}.${table.name}`.toLowerCase(), names);
      }
      return map;
    })
    .catch((): ColumnMap => new Map());
  columnCache = { sessionId, relations, columns };
  return columns;
}

function unique(columns: ColumnMap): string[] {
  return [...new Set([...columns.values()].flat())].sort();
}

function options(names: string[], type: string): Completion[] {
  return names.map((label) => ({ label, type }));
}

/** The dotted word before the cursor split into the owner chain and the part being typed. */
function dottedWord(context: CompletionContext) {
  const word = context.matchBefore(/[\w$."]*/);
  if (!word) return null;
  const parts = word.text.replace(/"/g, "").split(".");
  const partial = parts.pop() ?? "";
  return { from: word.to - partial.length, owner: parts.length ? parts.join(".") : null, partial };
}

/** Table behind an alias: FROM users u, JOIN users AS u. */
function tableOfAlias(doc: string, alias: string): string | null {
  const pattern = /\b(?:from|join|update|into)\s+([\w."]+)(?:\s+(?:as\s+)?(\w+))?/gi;
  for (const m of doc.matchAll(pattern)) {
    if (m[2]?.toLowerCase() === alias.toLowerCase()) return m[1].replace(/"/g, "");
  }
  return null;
}

function sqlSource(relations: Relation[]): CompletionSource {
  return async (context) => {
    const typed = dottedWord(context);
    if (!typed) return null;
    const { owner, partial } = typed;
    if (!owner && partial.length < 2) return null;
    const columns = await columnsFor(relations);
    let names: string[];
    if (owner) {
      const key = owner.toLowerCase();
      const aliased = tableOfAlias(context.state.doc.toString(), owner)?.toLowerCase();
      names = columns.get(key) ?? (aliased ? columns.get(aliased) : undefined) ?? [];
      if (!names.length && !relations.some((r) => r.name.toLowerCase() === key)) return null;
    } else {
      names = unique(columns);
    }
    return { from: typed.from, options: options(names, "property"), validFor: /^[\w$]*$/ } satisfies CompletionResult;
  };
}

function redisSource(relations: Relation[]): CompletionSource {
  return (context) => {
    const word = context.matchBefore(/[^\s]*/);
    if (!word || (word.from === word.to && !context.explicit)) return null;
    const line = context.state.doc.lineAt(context.pos);
    const first = line.text.slice(0, word.from - line.from).trim() === "";
    const list = first
      ? options(REDIS_COMMANDS, "keyword")
      : options([...new Set(relations.filter((r) => r.kind === "key").map((r) => r.name))].slice(0, 500), "variable");
    return { from: word.from, options: list, validFor: /^[^\s]*$/ };
  };
}

function mongoSource(relations: Relation[]): CompletionSource {
  return (context) => {
    const word = context.matchBefore(/[\w$.]*/);
    if (!word || (word.from === word.to && !context.explicit)) return null;
    const parts = word.text.split(".");
    const partial = parts.pop() ?? "";
    const from = word.to - partial.length;
    if (partial.startsWith("$")) return { from, options: options(MONGO_OPERATORS, "keyword") };
    if (parts.length === 1 && parts[0] === "db") {
      const collections = [...new Set(relations.map((r) => r.name))];
      return { from, options: [...options(collections, "class"), ...options(MONGO_DB_METHODS, "function")] };
    }
    if (parts.length === 2 && parts[0] === "db") return { from, options: options(MONGO_METHODS, "function") };
    return null;
  };
}

function language(engine: Engine | null, relations: Relation[]) {
  const completions = (source: CompletionSource) => EditorState.languageData.of(() => [{ autocomplete: source }]);
  if (engine === "redis") return [redisLanguage, completions(redisSource(relations))];
  if (engine === "mongodb") return [mongoLanguage, completions(mongoSource(relations))];
  const dialect = engine === "mysql" ? MySQL : engine === "sqlite" ? SQLite : PostgreSQL;
  const schema: NonNullable<SQLConfig["schema"]> = {};
  for (const r of relations) {
    const name = r.schema && engine === "postgres" && r.schema !== "public" ? `${r.schema}.${r.name}` : r.name;
    schema[name] = [];
  }
  return [sql({ dialect, schema, upperCaseKeywords: false }), completions(sqlSource(relations))];
}

export interface SQLEditorProps {
  value: string;
  onChange: (text: string) => void;
  onSelectionChange: (selected: string) => void;
  engine: Engine | null;
  relations: Relation[];
  placeholder: string;
  onRun: () => void;
  onRehearse: () => void;
  onRunAtomically: () => void;
}

/** This window's SQL editor, while one is shown. */
let shown: EditorView | null = null;

/**
 * Before a run that didn't come through the editor's own keys (a menu click or
 * accelerator, a key outside the editor): lets the keys already typed land in
 * the editor first (they can still be on their way when a native menu fires),
 * then closes its completion list, so the run takes the whole text.
 */
export async function settleSQLEditor(): Promise<void> {
  // A frame (none come while the window is hidden, hence the cap), then the
  // tasks queued behind it.
  await Promise.race([new Promise((done) => requestAnimationFrame(done)), new Promise((done) => setTimeout(done, 100))]);
  await new Promise((done) => setTimeout(done, 0));
  if (shown) closeCompletion(shown);
}

export function SQLEditor(props: SQLEditorProps) {
  const host = useRef<HTMLDivElement>(null);
  const view = useRef<EditorView | null>(null);
  const languageSlot = useRef(new Compartment());
  const placeholderSlot = useRef(new Compartment());
  // Callbacks change every render; the editor reads the latest through this.
  const latest = useRef(props);
  latest.current = props;

  useEffect(() => {
    const run = (action: "onRun" | "onRehearse" | "onRunAtomically") => (editor: EditorView) => {
      closeCompletion(editor);
      latest.current[action]();
      return true;
    };
    const state = EditorState.create({
      doc: props.value,
      extensions: [
        Prec.highest(
          keymap.of([
            { key: "Mod-Enter", run: run("onRun") },
            { key: "Mod-Alt-Enter", run: run("onRehearse") },
            { key: "Mod-Shift-Enter", run: run("onRunAtomically") },
          ]),
        ),
        lineNumbers(),
        highlightActiveLineGutter(),
        history(),
        drawSelection(),
        indentOnInput(),
        bracketMatching(),
        closeBrackets(),
        autocompletion({ icons: false }),
        highlightActiveLine(),
        syntaxHighlighting(highlight),
        keymap.of([...closeBracketsKeymap, ...defaultKeymap, ...historyKeymap, ...completionKeymap, indentWithTab]),
        languageSlot.current.of(language(props.engine, props.relations)),
        placeholderSlot.current.of(placeholderText(props.placeholder)),
        theme,
        EditorView.lineWrapping,
        EditorView.contentAttributes.of({ "aria-label": "SQL", spellcheck: "false" }),
        EditorView.updateListener.of((update) => {
          if (update.docChanged) latest.current.onChange(update.state.doc.toString());
          if (update.selectionSet || update.docChanged) {
            const { from, to } = update.state.selection.main;
            latest.current.onSelectionChange(update.state.sliceDoc(from, to));
          }
        }),
      ],
    });
    view.current = new EditorView({ state, parent: host.current! });
    shown = view.current;
    return () => {
      if (shown === view.current) shown = null;
      view.current?.destroy();
      view.current = null;
    };
    // The editor is made once; the effects below keep it in step.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Text set from outside (Query in SQL, a new connection).
  useEffect(() => {
    const editor = view.current;
    if (editor && editor.state.doc.toString() !== props.value) {
      editor.dispatch({ changes: { from: 0, to: editor.state.doc.length, insert: props.value } });
    }
  }, [props.value]);

  useEffect(() => {
    view.current?.dispatch({ effects: languageSlot.current.reconfigure(language(props.engine, props.relations)) });
  }, [props.engine, props.relations]);

  useEffect(() => {
    view.current?.dispatch({ effects: placeholderSlot.current.reconfigure(placeholderText(props.placeholder)) });
  }, [props.placeholder]);

  return <div className="sql-editor" ref={host} />;
}
