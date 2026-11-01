// The grid: any number of rows, drawn only where they are on screen. Click a
// row to select it (Shift and Ctrl/Cmd extend), click a header to sort,
// drag a header's edge to resize, double-click a cell to edit it in place,
// double-click a row number to open the row (ResultGrid.swift).

import { memo, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { Cell, ColumnSort, QueryResult } from "../api";
import { copyText, tsv } from "../format";
import { formatNumber, t } from "../i18n";
import { isMac } from "../shortcuts";
import { Icon } from "./Icon";

/** Matches --row-height in theme.css. */
const ROW = 24;
/** Rows drawn above and below the visible ones, so fast scrolling stays filled in. */
const OVERSCAN = 12;
const MIN_WIDTH = 56;
const MAX_WIDTH = 420;
/** Characters of a value drawn in a cell; the rest shows in the row editor. */
const SHOWN_CHARS = 300;

export interface GridProps {
  result: QueryResult;
  /** Changes when the rows are new: drops the edit in progress and scrolls to the top. */
  version: number;
  /** The first row's number minus one (the page offset). */
  startIndex: number;
  sort: ColumnSort | null;
  onSort: (sort: ColumnSort | null) => void;
  selection: Set<number>;
  onSelectionChange: (rows: Set<number>) => void;
  /** Set when cells can be edited. Rejects with the message to show. */
  onEditCell?: (row: number, column: number, value: Cell) => Promise<void>;
  onOpenRow?: (row: number) => void;
  onContextMenu?: (event: React.MouseEvent, row: number, column: number) => void;
  onCopied?: (rows: number) => void;
}

interface Editing {
  row: number;
  column: number;
  value: string;
  error?: string;
  saving?: boolean;
}

let measureCanvas: HTMLCanvasElement | null = null;

/** Column widths from the header and the first rows' text. */
function measure(result: QueryResult): number[] {
  measureCanvas ??= document.createElement("canvas");
  const context = measureCanvas.getContext("2d");
  const style = getComputedStyle(document.documentElement);
  const mono = `${style.getPropertyValue("--font-size-mono").trim() || "12px"} ${style.getPropertyValue("--font-mono").trim() || "monospace"}`;
  const ui = `600 ${style.getPropertyValue("--font-size-caption").trim() || "11px"} ${style.getPropertyValue("--font-ui").trim() || "sans-serif"}`;
  const width = (text: string, font: string) => {
    if (!context) return text.length * 7.2;
    context.font = font;
    return context.measureText(text).width;
  };
  const sample = result.rows.slice(0, 120);
  return result.columns.map((column, i) => {
    let widest = width(column.name, ui) + width(column.type, ui) + 28;
    for (const row of sample) {
      const value = row[i];
      widest = Math.max(widest, width(value === null ? "NULL" : value.slice(0, 80), mono) + 20);
      if (widest >= MAX_WIDTH) break;
    }
    return Math.round(Math.min(MAX_WIDTH, Math.max(MIN_WIDTH, widest)));
  });
}

function display(value: Cell): string {
  if (value === null) return "NULL";
  const short = value.length > SHOWN_CHARS ? `${value.slice(0, SHOWN_CHARS)}…` : value;
  return short.replace(/[\r\n\t]+/g, " ");
}

export const ResultGrid = memo(function ResultGrid(props: GridProps) {
  const { result, version, startIndex, sort, onSort, selection, onSelectionChange, onEditCell, onOpenRow, onContextMenu } = props;
  const scroller = useRef<HTMLDivElement>(null);
  const [scrollTop, setScrollTop] = useState(0);
  const [viewport, setViewport] = useState(600);
  const columnKey = result.columns.map((c) => `${c.name}:${c.type}`).join("|");
  const [widths, setWidths] = useState<number[]>(() => measure(result));
  const [editing, setEditing] = useState<Editing | null>(null);
  const anchor = useRef<number | null>(null);

  // New columns: measure again. New rows: start at the top, nothing in edit.
  useLayoutEffect(() => setWidths(measure(result)), [columnKey]); // eslint-disable-line react-hooks/exhaustive-deps
  useLayoutEffect(() => {
    setEditing(null);
    anchor.current = null;
    if (scroller.current) scroller.current.scrollTop = 0;
    setScrollTop(0);
  }, [version, columnKey]);

  useEffect(() => {
    const element = scroller.current;
    if (!element) return;
    const observer = new ResizeObserver(() => setViewport(element.clientHeight));
    observer.observe(element);
    setViewport(element.clientHeight);
    return () => observer.disconnect();
  }, []);

  const count = result.rows.length;
  const lastNumber = startIndex + count;
  const numberWidth = Math.max(44, String(lastNumber).length * 8 + 24);
  const template = useMemo(
    () => `${numberWidth}px ${widths.map((w) => `${w}px`).join(" ")} minmax(0, 1fr)`,
    [numberWidth, widths],
  );
  const total = numberWidth + widths.reduce((a, b) => a + b, 0);

  const first = Math.max(0, Math.floor(scrollTop / ROW) - OVERSCAN);
  const last = Math.min(count, Math.ceil((scrollTop + viewport) / ROW) + OVERSCAN);

  // MARK: Selection

  const select = useCallback(
    (row: number, event: { shiftKey: boolean; metaKey: boolean; ctrlKey: boolean }) => {
      const toggle = isMac ? event.metaKey : event.ctrlKey;
      let next: Set<number>;
      if (event.shiftKey && anchor.current !== null) {
        next = new Set(toggle ? selection : []);
        const [a, b] = [Math.min(anchor.current, row), Math.max(anchor.current, row)];
        for (let i = a; i <= b; i++) next.add(i);
      } else if (toggle) {
        next = new Set(selection);
        if (next.has(row)) next.delete(row);
        else next.add(row);
        anchor.current = row;
      } else {
        next = new Set([row]);
        anchor.current = row;
      }
      onSelectionChange(next);
    },
    [selection, onSelectionChange],
  );

  const reveal = (row: number) => {
    const element = scroller.current;
    if (!element) return;
    const top = (row + 1) * ROW;
    if (top - ROW < element.scrollTop) element.scrollTop = top - ROW;
    else if (top + ROW > element.scrollTop + element.clientHeight) element.scrollTop = top + ROW - element.clientHeight;
  };

  const onKeyDown = (event: React.KeyboardEvent) => {
    if (editing) return;
    const mod = isMac ? event.metaKey : event.ctrlKey;
    const current = anchor.current ?? (selection.size ? Math.max(...selection) : -1);
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      const row = Math.min(count - 1, Math.max(0, current + (event.key === "ArrowDown" ? 1 : -1)));
      if (row < 0) return;
      if (event.shiftKey) {
        const next = new Set(selection);
        next.add(row);
        onSelectionChange(next);
        anchor.current = row;
      } else {
        select(row, { shiftKey: false, metaKey: false, ctrlKey: false });
      }
      reveal(row);
    } else if (mod && event.key.toLowerCase() === "a") {
      event.preventDefault();
      onSelectionChange(new Set(result.rows.map((_, i) => i)));
    } else if (mod && event.key.toLowerCase() === "c" && selection.size > 0 && !event.shiftKey) {
      event.preventDefault();
      void copyText(tsv(result, [...selection], false)).then(() => props.onCopied?.(selection.size));
    } else if (event.key === "Enter" && onOpenRow && selection.size === 1) {
      event.preventDefault();
      onOpenRow([...selection][0]);
    } else if (event.key === "F2" && onEditCell && selection.size === 1) {
      event.preventDefault();
      const row = [...selection][0];
      setEditing({ row, column: 0, value: result.rows[row][0] ?? "" });
    } else if (event.key === "Escape" && selection.size > 0) {
      onSelectionChange(new Set());
    }
  };

  // MARK: Editing

  const commit = async (value: Cell) => {
    if (!editing || !onEditCell) return;
    const before = result.rows[editing.row]?.[editing.column] ?? null;
    if (value === before) {
      setEditing(null);
      scroller.current?.focus();
      return;
    }
    setEditing({ ...editing, saving: true, error: undefined });
    try {
      await onEditCell(editing.row, editing.column, value);
      setEditing(null);
      scroller.current?.focus();
    } catch (error) {
      setEditing({ ...editing, saving: false, error: typeof error === "string" ? error : String(error) });
    }
  };

  // MARK: Resizing

  const startResize = (index: number, event: React.MouseEvent) => {
    event.preventDefault();
    event.stopPropagation();
    const startX = event.clientX;
    const startWidth = widths[index];
    const move = (e: MouseEvent) => {
      const width = Math.max(MIN_WIDTH, Math.min(1200, startWidth + e.clientX - startX));
      setWidths((old) => old.map((w, i) => (i === index ? width : w)));
    };
    const up = () => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
      document.body.classList.remove("resizing-columns");
    };
    document.body.classList.add("resizing-columns");
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
  };

  const sortBy = (column: string) => {
    if (sort?.column !== column) onSort({ column, ascending: true });
    else if (sort.ascending) onSort({ column, ascending: false });
    else onSort(null);
  };

  const rows = [];
  for (let i = first; i < last; i++) {
    const row = result.rows[i];
    const selected = selection.has(i);
    rows.push(
      <div
        key={i}
        className={`grid-row${i % 2 ? " odd" : ""}${selected ? " selected" : ""}`}
        style={{ top: (i + 1) * ROW, gridTemplateColumns: template }}
        role="row"
        aria-rowindex={i + 2}
        aria-selected={selected}
        onMouseDown={(e) => {
          if (e.button === 0 && !(e.target as HTMLElement).closest("input")) select(i, e);
        }}
        onContextMenu={(e) => {
          if (!selection.has(i)) select(i, { shiftKey: false, metaKey: false, ctrlKey: false });
          const cell = (e.target as HTMLElement).closest("[data-col]") as HTMLElement | null;
          onContextMenu?.(e, i, cell ? Number(cell.dataset.col) : -1);
        }}
      >
        <div className="grid-number" role="rowheader" onDoubleClick={() => onOpenRow?.(i)} title={onOpenRow ? t("Double-click to open the row") : undefined}>
          {formatNumber(startIndex + i + 1)}
        </div>
        {row.map((value, c) =>
          editing && editing.row === i && editing.column === c ? (
            <div key={c} className="grid-cell editing" data-col={c} role="gridcell">
              <input
                autoFocus
                className={editing.error ? "invalid" : ""}
                value={editing.value}
                disabled={editing.saving}
                title={editing.error}
                spellCheck={false}
                onFocus={(e) => e.currentTarget.select()}
                onChange={(e) => setEditing({ ...editing, value: e.target.value, error: undefined })}
                onKeyDown={(e) => {
                  e.stopPropagation();
                  if (e.key === "Enter") void commit(editing.value);
                  if (e.key === "Escape") {
                    setEditing(null);
                    scroller.current?.focus();
                  }
                  if (e.key === "Tab") {
                    e.preventDefault();
                    const next = c + (e.shiftKey ? -1 : 1);
                    if (next >= 0 && next < row.length) setEditing({ row: i, column: next, value: row[next] ?? "" });
                  }
                }}
                onBlur={() => !editing.saving && !editing.error && setEditing(null)}
              />
              {editing.error && <div className="grid-cell-error selectable">{editing.error}</div>}
            </div>
          ) : (
            <div
              key={c}
              className={`grid-cell${value === null ? " null" : ""}`}
              data-col={c}
              role="gridcell"
              onDoubleClick={() => {
                if (onEditCell) setEditing({ row: i, column: c, value: value ?? "" });
                else onOpenRow?.(i);
              }}
            >
              {display(value)}
            </div>
          ),
        )}
        <div />
      </div>,
    );
  }

  return (
    <div
      className="grid-scroller"
      ref={scroller}
      tabIndex={0}
      role="grid"
      aria-rowcount={count + 1}
      aria-colcount={result.columns.length + 1}
      onScroll={(e) => setScrollTop(e.currentTarget.scrollTop)}
      onKeyDown={onKeyDown}
    >
      <div className="grid-canvas" style={{ height: (count + 1) * ROW, minWidth: total }}>
        <div className="grid-header" style={{ gridTemplateColumns: template }} role="row">
          <div className="grid-number header">#</div>
          {result.columns.map((column, i) => {
            const sorted = sort?.column === column.name ? (sort.ascending ? "ascending" : "descending") : undefined;
            return (
              <div
                key={`${column.name}-${i}`}
                className="grid-header-cell"
                role="columnheader"
                aria-sort={sorted ?? "none"}
                title={`${column.name}${column.type ? ` ${column.type}` : ""}`}
                onClick={() => sortBy(column.name)}
              >
                <span className="grid-header-name">{column.name}</span>
                {column.type && <span className="grid-header-type">{column.type}</span>}
                {sorted && <Icon name={sorted === "ascending" ? "sortUp" : "sortDown"} size={10} />}
                <span className="grid-resize" onMouseDown={(e) => startResize(i, e)} onClick={(e) => e.stopPropagation()} />
              </div>
            );
          })}
          <div />
        </div>
        {rows}
      </div>
    </div>
  );
});
