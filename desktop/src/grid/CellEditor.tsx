// The editor drawn over a cell: a text field with a NULL toggle. Enter keeps
// the change (pending until the row is saved), Shift+Enter starts a new line,
// Tab moves on, Escape drops it. An editor closed without typing changes
// nothing, so a NULL stays NULL; NULL is written only with the toggle (or the
// Set NULL action).

import { GridCellKind, type ProvideEditorComponent, type TextCell } from "@glideapps/glide-data-grid";
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { t } from "../i18n";

/** A grid cell with the database's NULL kept apart from empty text. */
export interface DbCell extends TextCell {
  readonly dbNull: boolean;
}

export function isDbCell(cell: { kind: GridCellKind }): cell is DbCell {
  return cell.kind === GridCellKind.Text && "dbNull" in cell;
}

/** Keys typed while the editor is still opening (its code loads on first use):
 *  the grid collects them here and the editor adds them after the first key. */
export const typeAhead = {
  opening: false,
  keys: "",
  since: 0,
  /** Enter, Tab, Shift+Tab or Escape pressed before the editor showed: done right after it does. */
  finish: "" as "" | "Enter" | "Tab" | "ShiftTab" | "Escape",
};

const moves: Record<string, readonly [-1 | 0 | 1, -1 | 0 | 1]> = { Enter: [0, 1], Tab: [1, 0], ShiftTab: [-1, 0] };

export const CellEditor: ProvideEditorComponent<DbCell> = ({ value, onChange, onFinishedEditing, initialValue: firstKey, target }) => {
  // Read here, cleared once mounted (React may run this twice in development).
  const [initialValue] = useState(() => (firstKey === undefined ? undefined : firstKey + (typeAhead.opening ? typeAhead.keys : "")));
  const [finish] = useState(() => (firstKey !== undefined && typeAhead.opening ? typeAhead.finish : ""));
  const replayed = useRef(false);
  useEffect(() => {
    typeAhead.opening = false;
    typeAhead.keys = "";
    typeAhead.finish = "";
    if (replayed.current) return;
    replayed.current = true;
    if (finish === "Escape") onFinishedEditing(undefined, [0, 0]);
    else if (finish && initialValue !== undefined) onFinishedEditing({ ...value, data: initialValue, dbNull: false }, moves[finish]);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  const [text, setText] = useState(() => initialValue ?? (value.dbNull ? "" : value.data));
  const [isNull, setNull] = useState(() => initialValue === undefined && value.dbNull);
  const area = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    // Typing on a cell opens the editor with that key: it is a change already.
    if (initialValue !== undefined) onChange({ ...value, data: initialValue, dbNull: false });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useLayoutEffect(() => {
    const el = area.current;
    if (!el) return;
    el.focus();
    const end = el.value.length;
    if (initialValue !== undefined) el.setSelectionRange(end, end);
    else el.select();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useLayoutEffect(() => {
    const el = area.current;
    if (!el) return;
    el.style.height = "0px";
    el.style.height = `${Math.min(160, Math.max(target.height, el.scrollHeight))}px`;
  }, [text, target.height]);

  const change = (next: string) => {
    setText(next);
    setNull(false);
    onChange({ ...value, data: next, dbNull: false });
  };

  const toggleNull = () => {
    const next = !isNull;
    setNull(next);
    if (next) setText("");
    onChange({ ...value, data: next ? "" : text, dbNull: next });
    area.current?.focus();
  };

  return (
    <div className="cell-editor" style={{ minWidth: Math.max(target.width, 220) }}>
      <textarea
        ref={area}
        className="cell-editor-text"
        value={text}
        placeholder={isNull ? "NULL" : undefined}
        spellCheck={false}
        rows={1}
        onChange={(e) => change(e.target.value)}
        onKeyDown={(e) => {
          // Mod+Shift+Delete writes NULL here too.
          if ((e.ctrlKey || e.metaKey) && e.shiftKey && (e.key === "Delete" || e.key === "Backspace")) {
            e.preventDefault();
            toggleNull();
          }
        }}
      />
      <div className="cell-editor-bar">
        <button
          type="button"
          className={`cell-editor-null${isNull ? " on" : ""}`}
          aria-pressed={isNull}
          title={t("Write NULL to this column")}
          onMouseDown={(e) => e.preventDefault()}
          onClick={toggleNull}
        >
          NULL
        </button>
        <span className="cell-editor-hint">{t("Enter keeps the change · Esc drops it")}</span>
      </div>
    </div>
  );
};
