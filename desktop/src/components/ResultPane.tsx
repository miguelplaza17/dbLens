// The center: the picked table a page at a time, or the last query's result,
// with the status line, paging, search, and row actions below (ResultPane.swift).

import { forwardRef, useCallback, useMemo, useState } from "react";
import { api, cellText, errorText, permuted, type ColumnSort, type ResultBlock } from "../api";
import { copyText, tsv } from "../format";
import { formatNumber, t } from "../i18n";
import { useModel } from "../model";
import { BlockData, COLLECT_CAP, type GridData } from "../grid/blocks";
import { gridCommand } from "../grid/commands";
import { isMac, keyLabel, withKeys } from "../shortcuts";

/** Saves the pending edits while the grid has the focus. */
const saveKey = isMac ? "⌘S" : "Ctrl+S";
import { Empty } from "./Empty";
import { Icon } from "./Icon";
import { canImportFromMac, importFromMac } from "../native/importSwift";
import { ResultGrid, type MenuEvent } from "./ResultGrid";

/** Sorts a query result on screen (a table page is sorted by the database). */
function sorted(result: ResultBlock, sort: ColumnSort | null): ResultBlock {
  if (!sort) return result;
  const i = result.columns.findIndex((c) => c.name === sort.column);
  if (i < 0) return result;
  const values = Array.from({ length: result.rowCount }, (_, r) => cellText(result, r, i));
  const numeric = values.every((v) => v === null || v === "" || !Number.isNaN(Number(v)));
  const order = values.map((_, r) => r).sort((a, b) => {
    const x = values[a];
    const y = values[b];
    if (x === y) return 0;
    if (x === null) return 1;
    if (y === null) return -1;
    const by = numeric ? Number(x) - Number(y) : x.localeCompare(y, undefined, { numeric: true });
    return sort.ascending ? by : -by;
  });
  return permuted(result, order);
}

export const ResultPane = forwardRef<HTMLInputElement>(function ResultPane(_props, searchRef) {
  const model = useModel();

  if (!model.session) {
    if (model.connectError) {
      return (
        <Empty icon="disconnect" title={t("Couldn't Connect")}>
          <span className="selectable pre">{model.connectError}</span>
        </Empty>
      );
    }
    if (model.connecting) {
      return (
        <Empty icon="database" title={t("Connecting…")}>
          <span className="spinner" />
        </Empty>
      );
    }
    return (
      <Empty icon="database" title={t("No Database Open")}>
        <p>{t("Choose a connection in the sidebar or paste the URL of a new database.")}</p>
        <button className="button prominent" onClick={() => model.open({ kind: "connection", editing: null })}>
          <Icon name="plus" size={14} />
          {t("New Connection…")}
          <kbd>{keyLabel("newConnection")}</kbd>
        </button>
        {canImportFromMac && model.store.connections.length === 0 && (
          <button className="button" onClick={() => void importFromMac(model)}>
            {t("Import Connections from dbLens for Mac…")}
          </button>
        )}
      </Empty>
    );
  }

  if (model.shown === "query") {
    const main = model.shownResult;
    if (!main) {
      return (
        <Empty icon="terminal" title={t("No Result")}>
          <p>{t("Write a query and press %@.", keyLabel("run"))}</p>
        </Empty>
      );
    }
    if (main.columns.length === 0) {
      return (
        <Empty icon="checkCircle" title={main.command || t("Done")}>
          <p>{t("The command returned no rows. See the log in the SQL panel.")}</p>
        </Empty>
      );
    }
    return <Grid result={main} ref={searchRef} />;
  }

  if (!model.relation) {
    return (
      <Empty icon="table" title={t("Choose a Table")}>
        <p>{t("%lld tables and views in this database. Or run a query in the SQL panel.", model.relations.length)}</p>
      </Empty>
    );
  }
  if (model.tableError) {
    return (
      <Empty icon="alert" title={t("Couldn't Read the Table")}>
        <span className="selectable pre">{model.tableError}</span>
      </Empty>
    );
  }
  if (!model.tableResult) {
    return (
      <div className="center-fill">
        <span className="spinner" />
      </div>
    );
  }
  return <Grid result={model.tableResult} ref={searchRef} />;
});

const Grid = forwardRef<HTMLInputElement, { result: ResultBlock }>(function Grid({ result }, searchRef) {
  const model = useModel();
  const isTable = model.shown === "table";
  const shown = useMemo(() => (isTable ? result : sorted(result, model.querySort)), [isTable, result, model.querySort]);
  const source = isTable ? model.tableSource : null;
  // A table reads its rows in blocks; a query result is one block, growing while it streams.
  const data: GridData = useMemo(() => source ?? new BlockData(shown), [source, shown]);
  const editable = model.canEditRows && !!source?.editable;
  const [shownRows, setShownRows] = useState<[number, number]>([0, 0]);

  const onSaveRow = useCallback((row: number) => model.saveRow(row), [model]);
  const onOpenRow = useCallback((row: number) => model.open({ kind: "row", row }), [model]);
  const onSort = useCallback((sort: ColumnSort | null) => void model.sort(sort), [model]);
  const onSelectionChange = useCallback((rows: Set<number>) => model.setSelection(rows), [model]);
  const onCopied = useCallback(
    (n: number, cut: boolean) => {
      const copied = n === 1 ? t("Copied 1 row") : t("Copied %@ rows", n);
      model.showToast(cut ? `${copied} · ${t("the first %@ only", formatNumber(COLLECT_CAP))}` : copied);
    },
    [model],
  );
  const onRowsShown = useCallback((first: number, last: number) => setShownRows([first, last]), []);

  const selected = [...model.selection].filter((i) => i < data.rows);

  const copy = async (rows?: number[]) => {
    const { block, cut } = await data.collect(rows ?? null);
    await copyText(tsv(block, undefined, true));
    onCopied(block.rowCount, cut);
  };

  const onContextMenu = (event: MenuEvent, row: number, column: number) => {
    const rows = model.selection.has(row) ? [...model.selection] : [row];
    const raw = column >= 0 ? data.shown(row, column) : undefined;
    const value = raw === undefined || raw === null ? raw : String(raw);
    const pending = data.hasPending;
    model.showMenu(event, [
      ...(column >= 0
        ? [{ title: t("Copy Value"), icon: "copy" as const, action: () => void copyText(value ?? "").then(() => model.showToast(t("Copied"))) }]
        : []),
      { title: rows.length === 1 ? t("Copy Row") : t("Copy %@ Rows", rows.length), icon: "copy", action: () => void copy(rows) },
      ...(editable
        ? [
            { separator: true, title: "" },
            ...(column >= 0 ? [{ title: withKeys(t("Set to NULL"), "setNull"), disabled: value === null, action: () => void gridCommand("setNull") }] : []),
            ...(pending
              ? [
                  { title: `${t("Save Changes")} (${saveKey})`, action: () => void gridCommand("saveChanges") },
                  { title: t("Revert Changes"), action: () => void gridCommand("revertChanges") },
                ]
              : []),
            { title: t("Edit Row…"), icon: "edit" as const, action: () => onOpenRow(row) },
            { title: rows.length === 1 ? t("Delete Row…") : t("Delete %@ Rows…", rows.length), icon: "trash" as const, destructive: true, action: () => confirmDelete(rows) },
          ]
        : []),
    ]);
  };

  const onHeaderMenu = (event: MenuEvent, column: number) => {
    const name = data.columns[column]?.name;
    if (!name) return;
    const sort = isTable ? model.tableSort : model.querySort;
    model.showMenu(event, [
      { title: t("Sort Ascending"), disabled: sort?.column === name && sort.ascending, action: () => void model.sort({ column: name, ascending: true }) },
      { title: t("Sort Descending"), disabled: sort?.column === name && !sort.ascending, action: () => void model.sort({ column: name, ascending: false }) },
      { title: t("Clear Sort"), disabled: !sort, action: () => void model.sort(null) },
      { separator: true, title: "" },
      { title: t("Freeze First Column"), action: () => void gridCommand("freezeFirstColumn") },
    ]);
  };

  const confirmDelete = (rows: number[]) => confirmDeleteRows(model, rows);

  const status = (() => {
    if (!isTable) return model.runSummary;
    const searching = model.tableSearch.trim() !== "";
    if (result.rowCount === 0) return searching ? t("No rows match") : result.note ?? t("Empty table");
    const exact = typeof model.tableCount === "number" || data.exact;
    const total = `${exact ? "" : "~"}${formatNumber(data.rows)}`;
    const [first, last] = shownRows;
    const rows = last > first ? t("Rows %@–%@ of %@", formatNumber(first + 1), formatNumber(Math.min(last, data.rows)), total) : t("%@ rows", total);
    return result.note ? `${rows} · ${result.note}` : rows;
  })();

  const exportAs = async (format: "csv" | "json") => {
    try {
      const { block } = await data.collect(null);
      const text = await api.exportResult(block, format);
      await copyText(text);
      model.showToast(format === "csv" ? t("Copied as CSV") : t("Copied as JSON"));
    } catch (e) {
      model.showToast(errorText(e));
    }
  };

  return (
    <div className="result">
      <ResultGrid
        data={data}
        version={isTable ? model.tableVersion : model.runVersion}
        sort={isTable ? model.tableSort : model.querySort}
        onSort={onSort}
        selection={model.selection}
        onSelectionChange={onSelectionChange}
        onSaveRow={editable ? onSaveRow : undefined}
        onOpenRow={editable ? onOpenRow : undefined}
        onContextMenu={onContextMenu}
        onHeaderMenu={onHeaderMenu}
        onCopied={onCopied}
        onRowsShown={onRowsShown}
      />
      <footer className="result-footer">
        <span className="status">{status}</span>
        {isTable && (model.tableLoading || (typeof model.tableCount !== "number" && !data.exact && model.tableCount !== null)) && (
          <span className="spinner small" title={t("Counting rows…")} />
        )}
        <span className="spacer" />
        {isTable && (
          <div className="search-field">
            <Icon name="search" size={12} />
            <input
              ref={searchRef}
              type="search"
              value={model.tableSearch}
              placeholder={t("Search rows")}
              title={withKeys(t("Rows with this text in any column, across every page"), "searchRows")}
              spellCheck={false}
              onChange={(e) => model.searchTable(e.target.value)}
              onKeyDown={(e) => e.key === "Escape" && model.searchTable("")}
            />
          </div>
        )}
        {isTable && !editable && model.relation?.kind !== "key" && (
          <span className="tertiary" title={t("Views and tables without a primary key can't be edited here.")}>
            {t("Read Only")}
          </span>
        )}
        {editable && data.hasPending && (
          <>
            <button className="button" onClick={() => void gridCommand("revertChanges")} title={t("Drop the edits not saved yet")}>
              {t("Revert")}
            </button>
            <button className="button prominent" onClick={() => void gridCommand("saveChanges")} title={`${t("Save the edited rows")} (${saveKey})`}>
              {t("Save Changes")}
            </button>
          </>
        )}
        {model.canInsertRows && (
          <button className="button" onClick={() => model.open({ kind: "row", row: null })} title={withKeys(t("Add a row"), "newRow")}>
            <Icon name="plus" size={14} />
            {t("New Row")}
          </button>
        )}
        {editable && selected.length > 0 && (
          <button className="button destructive" onClick={() => confirmDelete(selected)} title={withKeys(t("Delete the selected rows"), "deleteRows")}>
            <Icon name="trash" size={14} />
            {selected.length === 1 ? t("Delete Row…") : t("Delete %@ Rows…", selected.length)}
          </button>
        )}
        {selected.length > 0 ? (
          <button className="button" onClick={() => void copy(selected)} title={t("Copy the selected rows, with the header, to paste into a spreadsheet")}>
            <Icon name="copy" size={14} />
            {t("Copy Selected")}
          </button>
        ) : (
          <button
            className="button"
            onClick={() => void copy()}
            title={withKeys(t("Copy all rows, with the header, to paste into a spreadsheet"), "copyRows")}
          >
            <Icon name="copy" size={14} />
            {t("Copy All")}
          </button>
        )}
        <button
          className="button icon-only"
          aria-label={t("Export")}
          title={t("Export")}
          onClick={(e) =>
            model.showMenu({ clientX: e.currentTarget.getBoundingClientRect().left, clientY: e.currentTarget.getBoundingClientRect().top - 72, preventDefault: () => undefined }, [
              { title: t("Copy as CSV"), action: () => void exportAs("csv") },
              { title: t("Copy as JSON"), action: () => void exportAs("json") },
            ])
          }
        >
          <Icon name="export" size={14} />
        </button>
      </footer>
    </div>
  );
});

/** Asks, then deletes the rows (by index into the page). */
export function confirmDeleteRows(model: ReturnType<typeof useModel>, rows: number[]) {
  if (rows.length === 0 || !model.canEditRows) return;
  model.open({
    kind: "confirm",
    title: rows.length === 1 ? t("Delete this row?") : t("Delete %@ rows?", rows.length),
    message: t("The DELETE is sent to the database right away and can't be undone here."),
    action: t("Delete"),
    destructive: true,
    run: () => model.deleteRows(rows),
  });
}
