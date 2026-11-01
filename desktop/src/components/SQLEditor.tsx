// The editor in the SQL panel: CodeMirror 6 with SQL highlighting for the
// connection's dialect and table names to complete. Redis and MongoDB get a
// plain editor (commands). Mod-Enter runs; the colors come from theme.css.

import { useEffect, useRef } from "react";
import { autocompletion, closeBrackets, closeBracketsKeymap, completionKeymap } from "@codemirror/autocomplete";
import { defaultKeymap, history, historyKeymap, indentWithTab } from "@codemirror/commands";
import { MySQL, PostgreSQL, SQLite, sql, type SQLConfig } from "@codemirror/lang-sql";
import { bracketMatching, HighlightStyle, indentOnInput, syntaxHighlighting } from "@codemirror/language";
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
import type { Engine, Relation } from "../api";

const highlight = HighlightStyle.define([
  { tag: [tags.keyword, tags.operatorKeyword, tags.modifier], color: "var(--syntax-keyword)" },
  { tag: [tags.function(tags.variableName), tags.standard(tags.name)], color: "var(--syntax-function)" },
  { tag: [tags.string, tags.special(tags.string)], color: "var(--syntax-string)" },
  { tag: [tags.number, tags.bool, tags.null], color: "var(--syntax-number)" },
  { tag: [tags.lineComment, tags.blockComment, tags.comment], color: "var(--syntax-comment)", fontStyle: "italic" },
  { tag: [tags.typeName, tags.standard(tags.typeName)], color: "var(--syntax-type)" },
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

function language(engine: Engine | null, relations: Relation[]) {
  if (engine === "redis" || engine === "mongodb") return [];
  const dialect = engine === "mysql" ? MySQL : engine === "sqlite" ? SQLite : PostgreSQL;
  const schema: NonNullable<SQLConfig["schema"]> = {};
  for (const r of relations) {
    const name = r.schema && engine === "postgres" && r.schema !== "public" ? `${r.schema}.${r.name}` : r.name;
    schema[name] = [];
  }
  return sql({ dialect, schema, upperCaseKeywords: false });
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

export function SQLEditor(props: SQLEditorProps) {
  const host = useRef<HTMLDivElement>(null);
  const view = useRef<EditorView | null>(null);
  const languageSlot = useRef(new Compartment());
  const placeholderSlot = useRef(new Compartment());
  // Callbacks change every render; the editor reads the latest through this.
  const latest = useRef(props);
  latest.current = props;

  useEffect(() => {
    const run = (action: "onRun" | "onRehearse" | "onRunAtomically") => () => {
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
    return () => {
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
