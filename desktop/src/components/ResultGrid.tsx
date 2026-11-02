// The grid: a canvas (Glide Data Grid) that draws only the cells on screen.
// A table's rows come a block at a time from src/grid/blocks.ts as the grid
// scrolls; a query result is the block the run streamed in. Click a header to
// sort, drag its edge to resize; type or double-click on a cell to edit it
// (the change waits, highlighted, until the row is saved: moving to another
// row or Ctrl/Cmd+S); Escape reverts the row (ResultGrid.swift).

import "@glideapps/glide-data-grid/dist/index.css";
import "../grid/grid.css";
import {
  CompactSelection,
  DataEditorCore,
  GridCellKind,
  ImageWindowLoaderImpl,
  loadingCellRenderer,
  markerCellRenderer,
  newRowCellRenderer,
  textCellRenderer,
  type DataEditorCoreProps,
  type DataEditorRef,
  type DrawHeaderCallback,
  type EditableGridCell,
  type GridCell,
  type GridColumn,
  type GridSelection,
  type Item,
  type Rectangle,
  type Theme,
} from "@glideapps/glide-data-grid";
import { memo, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { ColumnSort } from "../api";
import { copyText, tsv } from "../format";
import { t } from "../i18n";
import { isMac } from "../shortcuts";
import { CellEditor, isDbCell, typeAhead, type DbCell } from "../grid/CellEditor";
import type { Change, GridData } from "../grid/blocks";
import { registerGrid, SET_NULL_EVENT, type GridCommand } from "../grid/commands";
import { dispatch } from "../native/actions";
import { markFirstRows, recordScroll } from "../grid/perf";
import { readGridColors, type GridColors } from "../grid/theme";

/** Matches --row-height in theme.css. */
const ROW = 24;
const HEADER = 28;
const MIN_WIDTH = 56;
const MAX_WIDTH = 420;
/** Characters of a value drawn in a cell; the rest shows in the editor. */
const SHOWN_CHARS = 300;
/** Most cells Set NULL changes at once. */
const MAX_NULL_CELLS = 10_000;
const FREEZE_KEY = "grid.freezeFirstColumn";

// Only the cell kinds the grid draws (the full set pulls in markdown and images).
const renderers = [textCellRenderer, loadingCellRenderer, markerCellRenderer, newRowCellRenderer] as unknown as DataEditorCoreProps["renderers"];

// The overlay editor renders into #portal.
if (typeof document !== "undefined" && !document.getElementById("portal")) {
  const portal = document.createElement("div");
  portal.id = "portal";
  portal.className = "grid-portal";
  document.body.appendChild(portal);
}

export interface MenuEvent {
  clientX: number;
  clientY: number;
  preventDefault(): void;
}

export interface GridProps {
  data: GridData;
  /** Changes when the rows are new: drops the selection and scrolls to the top. */
  version: number;
  sort: ColumnSort | null;
  onSort: (sort: ColumnSort | null) => void;
  selection: Set<number>;
  onSelectionChange: (rows: Set<number>) => void;
  /** Saves the row's pending edits (only the changed columns). Set when cells can be edited. */
  onSaveRow?: (row: number) => Promise<void>;
  onOpenRow?: (row: number) => void;
  onContextMenu?: (event: MenuEvent, row: number, column: number) => void;
  onHeaderMenu?: (event: MenuEvent, column: number) => void;
  onCopied?: (rows: number, cut: boolean) => void;
  /** Rows on screen, for the status line. */
  onRowsShown?: (first: number, last: number) => void;
}

function stored(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function store(key: string, value: string) {
  try {
    localStorage.setItem(key, value);
  } catch {
    // Private windows: lasts until the window closes.
  }
}

let measureContext: CanvasRenderingContext2D | null = null;

/** Column widths from the header and the first loaded rows' text. */
function measure(data: GridData, colors: GridColors): number[] {
  measureContext ??= document.createElement("canvas").getContext("2d");
  const context = measureContext;
  const mono = `${colors.monoSize} ${colors.fontMono}`;
  const ui = `600 ${colors.captionSize} ${colors.fontUI}`;
  const width = (text: string, font: string) => {
    if (!context) return text.length * 7.2;
    context.font = font;
    return context.measureText(text).width;
  };
  const sample = Math.min(120, data.rows);
  return data.columns.map((column, c) => {
    let widest = width(column.name, ui) + width(column.type, ui) + 32;
    for (let row = 0; row < sample; row++) {
      const value = data.value(row, c);
      if (value === undefined) break;
      const text = value === null ? "NULL" : String(value).slice(0, 80);
      widest = Math.max(widest, width(text, mono) + 18);
      if (widest >= MAX_WIDTH) break;
    }
    return Math.round(Math.min(MAX_WIDTH, Math.max(MIN_WIDTH, widest)));
  });
}

function display(text: string): string {
  const short = text.length > SHOWN_CHARS ? `${text.slice(0, SHOWN_CHARS)}…` : text;
  return /[\r\n\t]/.test(short) ? short.replace(/[\r\n\t]+/g, " ") : short;
}

/** The rows a selection covers: picked rows, else the cell ranges' rows. */
function selectedRows(selection: GridSelection): number[] {
  if (selection.rows.length > 0) return selection.rows.toArray();
  const current = selection.current;
  if (!current) return [];
  const rows = new Set<number>();
  for (const range of [current.range, ...current.rangeStack]) {
    for (let r = range.y; r < range.y + range.height; r++) rows.add(r);
  }
  return [...rows].sort((a, b) => a - b);
}

const emptySelection: GridSelection = { columns: CompactSelection.empty(), rows: CompactSelection.empty() };

/** Watches html[data-theme] so the canvas recolors with the window. */
function useGridColors(): GridColors {
  const [colors, setColors] = useState(readGridColors);
  useEffect(() => {
    const observer = new MutationObserver(() => setColors(readGridColors()));
    observer.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme", "style", "class"] });
    return () => observer.disconnect();
  }, []);
  return colors;
}

export const ResultGrid = memo(function ResultGrid(props: GridProps) {
  const { data, version, sort, onSort, onSelectionChange, onSaveRow, onOpenRow, onContextMenu, onHeaderMenu, onCopied, onRowsShown } = props;
  const editor = useRef<DataEditorRef>(null);
  const container = useRef<HTMLDivElement>(null);
  const colors = useGridColors();
  const [tick, redraw] = useState(0);
  const [selection, setSelection] = useState<GridSelection>(emptySelection);
  const [freeze, setFreeze] = useState(() => stored(FREEZE_KEY) === "1");
  const visible = useRef<Rectangle>({ x: 0, y: 0, width: 0, height: 0 });
  const columnKey = data.columns.map((c) => `${c.name}:${c.type}`).join("|");
  const [widths, setWidths] = useState<number[]>(() => measure(data, colors));
  /** The row the cursor is on; leaving it saves its pending edits. */
  const editRow = useRef<number | null>(null);
  const editable = data.editable && !!onSaveRow;
  const imageWindowLoader = useMemo(() => new ImageWindowLoaderImpl(), []);

  // New columns: measure again.
  useLayoutEffect(() => setWidths(measure(data, colors)), [columnKey]); // eslint-disable-line react-hooks/exhaustive-deps

  // New rows: back to the top, nothing selected.
  useLayoutEffect(() => {
    setSelection(emptySelection);
    editRow.current = null;
    editor.current?.scrollTo(0, 0, "vertical");
  }, [version, columnKey]);

  // Blocks arriving redraw just their cells on screen; a new row count redraws all.
  useEffect(() => {
    return data.subscribe((change: Change) => {
      if (!change) {
        redraw((n) => n + 1);
        return;
      }
      const v = visible.current;
      const from = Math.max(change.from, v.y);
      const to = Math.min(change.to, v.y + v.height + 1);
      if (from >= to) return;
      const cells: { cell: Item }[] = [];
      for (let r = from; r < to; r++) for (let c = 0; c < data.columns.length; c++) cells.push({ cell: [c, r] });
      editor.current?.updateCells(cells);
      if (data.value(v.y, 0) !== undefined || data.columns.length === 0) markFirstRows();
    });
  }, [data]);

  useEffect(() => {
    if (data.rows > 0 && data.value(visible.current.y, 0) !== undefined) markFirstRows();
  }, [data, version]);

  // MARK: Cells

  const themes = useMemo(() => {
    const nullTheme: Partial<Theme> = { textDark: colors.null };
    return {
      null: nullTheme,
      pending: { bgCell: colors.pending } as Partial<Theme>,
      pendingNull: { bgCell: colors.pending, textDark: colors.null } as Partial<Theme>,
      stripe: { bgCell: colors.stripe } as Partial<Theme>,
      failed: { bgCell: colors.failed } as Partial<Theme>,
    };
  }, [colors]);

  const loading = useMemo<GridCell>(() => ({ kind: GridCellKind.Loading, allowOverlay: false }), []);

  const getCellContent = useCallback(
    ([col, row]: Item): GridCell => {
      const value = data.shown(row, col);
      if (value === undefined) return loading;
      const pending = data.pending(row, col) !== undefined;
      if (value === null) {
        const cell: DbCell = {
          kind: GridCellKind.Text,
          data: "",
          displayData: "NULL",
          dbNull: true,
          allowOverlay: true,
          readonly: !editable,
          copyData: "",
          themeOverride: pending ? themes.pendingNull : themes.null,
        };
        return cell;
      }
      const text = typeof value === "string" ? value : String(value);
      const cell: DbCell = {
        kind: GridCellKind.Text,
        data: text,
        displayData: display(text),
        dbNull: false,
        allowOverlay: true,
        readonly: !editable,
        contentAlign: typeof value === "number" ? "right" : undefined,
        themeOverride: pending ? themes.pending : undefined,
      };
      return cell;
    },
    // A new `tick` re-creates this, so the grid repaints everything.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [data, editable, themes, loading, data.rows, tick],
  );

  const getRowThemeOverride = useCallback(
    (row: number): Partial<Theme> | undefined => (data.failure(row) !== undefined ? themes.failed : row % 2 ? themes.stripe : undefined),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [data, themes, tick],
  );

  const columns = useMemo<GridColumn[]>(
    () => data.columns.map((c, i) => ({ id: `${i}`, title: c.name, width: widths[i] ?? 120 })),
    [data.columns, widths],
  );

  // MARK: Header

  const drawHeader = useCallback<DrawHeaderCallback>(
    ({ ctx, rect, columnIndex }) => {
      const column = data.columns[columnIndex];
      if (!column) return;
      const sorted = sort?.column === column.name ? (sort.ascending ? "asc" : "desc") : null;
      const pad = 8;
      const arrow = sorted ? 12 : 0;
      const mid = rect.y + rect.height / 2;
      ctx.save();
      ctx.beginPath();
      ctx.rect(rect.x, rect.y, rect.width - 1, rect.height);
      ctx.clip();
      ctx.textBaseline = "middle";
      ctx.font = `600 ${colors.captionSize} ${colors.fontUI}`;
      ctx.fillStyle = colors.text;
      let x = rect.x + pad;
      const room = rect.width - pad * 2 - arrow;
      const name = fit(ctx, column.name, room);
      ctx.fillText(name, x, mid);
      x += ctx.measureText(name).width + 6;
      if (column.type && x < rect.x + rect.width - pad - arrow - 12) {
        ctx.font = `${colors.captionSize} ${colors.fontUI}`;
        ctx.fillStyle = colors.textTertiary;
        ctx.fillText(fit(ctx, column.type, rect.x + rect.width - pad - arrow - x), x, mid);
      }
      if (sorted) {
        const ax = rect.x + rect.width - pad - 4;
        ctx.fillStyle = colors.accentText;
        ctx.beginPath();
        if (sorted === "asc") {
          ctx.moveTo(ax - 4, mid + 2);
          ctx.lineTo(ax + 4, mid + 2);
          ctx.lineTo(ax, mid - 3);
        } else {
          ctx.moveTo(ax - 4, mid - 2);
          ctx.lineTo(ax + 4, mid - 2);
          ctx.lineTo(ax, mid + 3);
        }
        ctx.fill();
      }
      ctx.restore();
    },
    [data.columns, sort, colors],
  );

  const onHeaderClicked = useCallback(
    (col: number) => {
      const name = data.columns[col]?.name;
      if (!name) return;
      if (sort?.column !== name) onSort({ column: name, ascending: true });
      else if (sort.ascending) onSort({ column: name, ascending: false });
      else onSort(null);
    },
    [data.columns, sort, onSort],
  );

  const onColumnResize = useCallback((_column: GridColumn, size: number, index: number) => {
    setWidths((old) => old.map((w, i) => (i === index ? Math.max(MIN_WIDTH, Math.round(size)) : w)));
  }, []);

  // MARK: Selection and saving

  const saveRow = useCallback(
    (row: number | null) => {
      if (row === null || !onSaveRow || !data.editable) return;
      if (data.changes(row).length > 0) void onSaveRow(row);
    },
    [data, onSaveRow],
  );

  const onGridSelectionChange = useCallback(
    (next: GridSelection) => {
      setSelection(next);
      const row = next.current?.cell[1] ?? null;
      if (editRow.current !== null && row !== editRow.current) saveRow(editRow.current);
      editRow.current = row;
      onSelectionChange(new Set(selectedRows(next)));
    },
    [onSelectionChange, saveRow],
  );

  // Rows deselected from outside (a delete, the Escape key in the window): clear here too.
  useEffect(() => {
    if (props.selection.size === 0 && (selection.current || selection.rows.length > 0)) setSelection(emptySelection);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [props.selection]);

  const onCellEdited = useCallback(
    ([col, row]: Item, value: EditableGridCell) => {
      if (!isDbCell(value) || !data.editable) return;
      data.edit(row, col, value.dbNull ? null : value.data);
    },
    [data],
  );

  const provideEditor = useCallback((cell: GridCell) => {
    if (!isDbCell(cell) || cell.readonly) return undefined;
    return { editor: CellEditor, disablePadding: true, disableStyling: true };
  }, []);

  // MARK: Copy

  /** The selection as TSV: picked rows whole, a cell range as its rectangle. */
  const copySelection = useCallback(async () => {
    const current = selection.current;
    if (selection.rows.length > 0) {
      const rows = selection.rows.toArray();
      const { block, cut } = await data.collect(rows);
      await copyText(tsv(block, undefined, false));
      onCopied?.(block.rowCount, cut);
      return;
    }
    if (!current) return;
    const { x, y, width, height } = current.range;
    const rows = Array.from({ length: height }, (_, i) => y + i);
    const { block, cut } = await data.collect(rows);
    const lines = block.data[0]
      ? Array.from({ length: block.rowCount }, (_, r) =>
          Array.from({ length: width }, (_, c) => (block.data[x + c]?.[r] ?? "").toString().replace(/[\t\r\n]+/g, " ")).join("\t"),
        )
      : [];
    await copyText(lines.join("\n"));
    onCopied?.(block.rowCount, cut);
  }, [data, selection, onCopied]);

  const copyAll = useCallback(async () => {
    const { block, cut } = await data.collect(null);
    await copyText(tsv(block, undefined, true));
    onCopied?.(block.rowCount, cut);
  }, [data, onCopied]);

  // MARK: Commands (shortcuts and menus)

  const setNull = useCallback(() => {
    const current = selection.current;
    if (!current || !editable) return false;
    const { x, y, width, height } = current.range;
    if (width * height > MAX_NULL_CELLS) return false;
    for (let r = y; r < y + height; r++) for (let c = x; c < x + width; c++) if (data.value(r, c) !== undefined) data.edit(r, c, null);
    return true;
  }, [data, editable, selection]);

  const saveAll = useCallback(async () => {
    if (!onSaveRow) return;
    for (const row of data.pendingRows()) await onSaveRow(row);
  }, [data, onSaveRow]);

  const revert = useCallback(
    (row?: number) => {
      if (!data.hasPending) return false;
      if (row !== undefined && data.changes(row).length === 0) return false;
      data.revert(row);
      return true;
    },
    [data],
  );

  const page = useCallback((direction: 1 | -1) => {
    const v = visible.current;
    const target = Math.max(0, Math.min(data.rows - 1, v.y + direction * Math.max(1, v.height - 1)));
    editor.current?.scrollTo(0, target, "vertical", 0, 0, { vAlign: "start" });
  }, [data.rows]);

  const toggleFreeze = useCallback(() => {
    setFreeze((f) => {
      store(FREEZE_KEY, f ? "0" : "1");
      return !f;
    });
  }, []);

  useEffect(
    () =>
      registerGrid((command: GridCommand) => {
        switch (command) {
          case "saveChanges":
            void saveAll();
            return true;
          case "revertChanges":
            return revert();
          case "setNull":
            return setNull();
          case "copySelection":
            void copySelection();
            return true;
          case "copyAll":
            void copyAll();
            return true;
          case "freezeFirstColumn":
            toggleFreeze();
            return true;
          case "pageUp":
            page(-1);
            return true;
          case "pageDown":
            page(1);
            return true;
        }
      }),
    [saveAll, revert, setNull, copySelection, copyAll, toggleFreeze, page],
  );

  // The menu's Set NULL.
  useEffect(() => {
    const onSetNull = () => void setNull();
    window.addEventListener(SET_NULL_EVENT, onSetNull);
    return () => window.removeEventListener(SET_NULL_EVENT, onSetNull);
  }, [setNull]);

  // Keys the canvas doesn't handle itself (it keeps arrows, typing, Enter, Tab).
  const onKeyDownCapture = (event: React.KeyboardEvent) => {
    const target = event.target as HTMLElement;
    if (target.closest(".cell-editor")) return;
    const mod = isMac ? event.metaKey : event.ctrlKey;
    if (typeAhead.opening && ["Enter", "Tab", "Escape"].includes(event.key) && performance.now() - typeAhead.since < 1000) {
      // The editor is still opening: it does this key once it shows.
      typeAhead.finish = event.key === "Tab" && event.shiftKey ? "ShiftTab" : (event.key as "Enter" | "Tab" | "Escape");
      event.preventDefault();
      event.stopPropagation();
      return;
    }
    if (editable && !event.ctrlKey && !event.metaKey && !event.altKey && event.key.length === 1 && selection.current) {
      // The first key opens the editor; keys typed before it shows are kept for it.
      const now = performance.now();
      if (typeAhead.opening && now - typeAhead.since < 1000) {
        typeAhead.keys += event.key;
        event.preventDefault();
        event.stopPropagation();
      } else {
        typeAhead.opening = true;
        typeAhead.keys = "";
        typeAhead.finish = "";
        typeAhead.since = now;
      }
      return;
    }
    if (mod && !event.shiftKey && !event.altKey && event.key.toLowerCase() === "c") {
      event.preventDefault();
      event.stopPropagation();
      void copySelection();
    } else if (mod && !event.shiftKey && !event.altKey && event.key.toLowerCase() === "s") {
      event.preventDefault();
      event.stopPropagation();
      // Through the dispatcher, so the menu's Ctrl/Cmd+S accelerator firing too saves once.
      dispatch("saveChanges");
    } else if (event.key === "Escape") {
      const row = selection.current?.cell[1];
      if (revert(row)) {
        event.preventDefault();
        event.stopPropagation();
      }
    } else if (mod && !event.altKey && event.key.toLowerCase() === "a") {
      // Every row: rows selected as a range of the first column through the last.
      event.preventDefault();
      event.stopPropagation();
      if (data.rows === 0 || data.columns.length === 0) return;
      onGridSelectionChange({
        columns: CompactSelection.empty(),
        rows: CompactSelection.fromSingleSelection([0, data.rows]),
        current: undefined,
      });
    }
  };

  // MARK: Mouse

  const onCellContextMenu = useCallback(
    ([col, row]: Item, event: { bounds: Rectangle; localEventX: number; localEventY: number; preventDefault(): void }) => {
      event.preventDefault();
      if (!onContextMenu) return;
      const inside = selection.rows.hasIndex(row) || (selection.current && rectHas(selection.current.range, col, row));
      if (!inside) {
        onGridSelectionChange({
          columns: CompactSelection.empty(),
          rows: CompactSelection.empty(),
          current: { cell: [col, row], range: { x: col, y: row, width: 1, height: 1 }, rangeStack: [] },
        });
      }
      onContextMenu({ clientX: event.bounds.x + event.localEventX, clientY: event.bounds.y + event.localEventY, preventDefault: () => undefined }, row, col);
    },
    [onContextMenu, selection, onGridSelectionChange],
  );

  const onHeaderContextMenu = useCallback(
    (col: number, event: { bounds: Rectangle; localEventX: number; localEventY: number; preventDefault(): void }) => {
      event.preventDefault();
      onHeaderMenu?.({ clientX: event.bounds.x + event.localEventX, clientY: event.bounds.y + event.localEventY, preventDefault: () => undefined }, col);
    },
    [onHeaderMenu],
  );

  const onCellActivated = useCallback(
    ([, row]: Item) => {
      if (!editable) onOpenRow?.(row);
    },
    [editable, onOpenRow],
  );

  const onVisibleRegionChanged = useCallback(
    (range: Rectangle) => {
      const previous = visible.current;
      visible.current = range;
      if (previous.y !== range.y || previous.height !== range.height) {
        recordScroll();
        data.view(range.y, range.y + range.height);
        onRowsShown?.(range.y, Math.min(data.rows, range.y + range.height));
      }
    },
    [data, onRowsShown],
  );

  // A new source: tell it what is on screen.
  useEffect(() => {
    const v = visible.current;
    data.view(v.y, v.y + Math.max(v.height, 40));
  }, [data]);

  const lastNumber = data.rows;
  const markerWidth = Math.max(44, String(lastNumber).length * 8 + 20);

  return (
    <div className="glide-grid" ref={container} onKeyDownCapture={onKeyDownCapture}>
      <DataEditorCore
        ref={editor}
        width="100%"
        height="100%"
        columns={columns}
        rows={data.rows}
        getCellContent={getCellContent}
        getRowThemeOverride={getRowThemeOverride}
        renderers={renderers}
        imageWindowLoader={imageWindowLoader}
        theme={colors.theme}
        rowHeight={ROW}
        headerHeight={HEADER}
        rowMarkers={{ kind: "clickable-number", startIndex: 1, width: markerWidth, theme: { textLight: colors.textTertiary, textMedium: colors.textSecondary } }}
        freezeColumns={freeze && columns.length > 1 ? 1 : 0}
        smoothScrollX
        smoothScrollY
        minColumnWidth={MIN_WIDTH}
        maxColumnWidth={1200}
        drawHeader={drawHeader}
        onHeaderClicked={onHeaderClicked}
        onHeaderContextMenu={onHeaderContextMenu}
        onColumnResize={onColumnResize}
        gridSelection={selection}
        onGridSelectionChange={onGridSelectionChange}
        rowSelect="multi"
        columnSelect="none"
        rangeSelect="rect"
        rowSelectionMode="auto"
        onCellEdited={onCellEdited}
        provideEditor={provideEditor as never}
        onCellContextMenu={onCellContextMenu}
        onCellActivated={onCellActivated}
        cellActivationBehavior="double-click"
        editOnType={editable}
        onVisibleRegionChanged={onVisibleRegionChanged}
        keybindings={{ copy: false, paste: false, cut: false, delete: false, search: false, clear: false, downFill: false, rightFill: false }}
        fixedShadowX
        fixedShadowY={false}
      />
      {data.error && (
        <div className="grid-error selectable" role="alert">
          {t("Couldn't Read the Table")}: {data.error}
        </div>
      )}
    </div>
  );
});

function rectHas(r: Rectangle, col: number, row: number): boolean {
  return col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height;
}

/** The text cut with … to fit `room` pixels. */
function fit(ctx: CanvasRenderingContext2D, text: string, room: number): string {
  if (ctx.measureText(text).width <= room) return text;
  let lo = 0;
  let hi = text.length;
  while (lo < hi) {
    const mid = (lo + hi + 1) >> 1;
    if (ctx.measureText(`${text.slice(0, mid)}…`).width <= room) lo = mid;
    else hi = mid - 1;
  }
  return lo > 0 ? `${text.slice(0, lo)}…` : "";
}
