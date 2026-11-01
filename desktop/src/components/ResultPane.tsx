// The center: the picked table a page at a time, or the last query's result,
// with the status line, paging, search, and row actions below (ResultPane.swift).

import { forwardRef, useCallback, useMemo } from "react";
import { api, errorText, type Cell, type ColumnSort, type QueryResult } from "../api";
import { copyText, tsv } from "../format";
import { formatNumber, t } from "../i18n";
import { pageSizes, useModel } from "../model";
import { keyLabel, withKeys } from "../shortcuts";
import { Empty } from "./Empty";
import { Icon } from "./Icon";
import { ResultGrid } from "./ResultGrid";

/** Sorts a query result on screen (a table page is sorted by the database). */
function sorted(result: QueryResult, sort: ColumnSort | null): QueryResult {
  if (!sort) return result;
  const i = result.columns.findIndex((c) => c.name === sort.column);
  if (i < 0) return result;
  const numeric = result.rows.every((r) => r[i] === null || r[i] === "" || !Number.isNaN(Number(r[i])));
  const rows = [...result.rows].sort((a, b) => {
    const x = a[i];
    const y = b[i];
    if (x === y) return 0;
    if (x === null) return 1;
    if (y === null) return -1;
    const order = numeric ? Number(x) - Number(y) : x.localeCompare(y, undefined, { numeric: true });
    return sort.ascending ? order : -order;
  });
  return { ...result, rows };
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

const Grid = forwardRef<HTMLInputElement, { result: QueryResult }>(function Grid({ result }, searchRef) {
  const model = useModel();
  const isTable = model.shown === "table";
  const shown = useMemo(() => (isTable ? result : sorted(result, model.querySort)), [isTable, result, model.querySort]);
  const editable = model.canEditRows;

  const onEditCell = useCallback(async (row: number, column: number, value: Cell) => {
    try {
      await model.updateCell(row, column, value);
    } catch (e) {
      throw errorText(e);
    }
  }, [model]);
  const onOpenRow = useCallback((row: number) => model.open({ kind: "row", row }), [model]);
  const onSort = useCallback((sort: ColumnSort | null) => void model.sort(sort), [model]);
  const onSelectionChange = useCallback((rows: Set<number>) => model.setSelection(rows), [model]);
  const onCopied = useCallback((n: number) => model.showToast(n === 1 ? t("Copied 1 row") : t("Copied %@ rows", n)), [model]);

  const selected = [...model.selection].filter((i) => i < shown.rows.length);

  const copy = async (rows?: number[]) => {
    await copyText(tsv(shown, rows, true));
    const n = rows?.length ?? shown.rows.length;
    onCopied(n);
  };

  const onContextMenu = (event: React.MouseEvent, row: number, column: number) => {
    const rows = model.selection.has(row) ? [...model.selection] : [row];
    const value = column >= 0 ? shown.rows[row]?.[column] : undefined;
    model.showMenu(event, [
      ...(column >= 0
        ? [{ title: t("Copy Value"), icon: "copy" as const, action: () => void copyText(value ?? "").then(() => model.showToast(t("Copied"))) }]
        : []),
      { title: rows.length === 1 ? t("Copy Row") : t("Copy %@ Rows", rows.length), icon: "copy", action: () => void copy(rows) },
      ...(editable
        ? [
            { separator: true, title: "" },
            ...(column >= 0
              ? [{ title: t("Set to NULL"), disabled: value === null, action: () => void onEditCell(row, column, null).catch((e) => model.showToast(String(e))) }]
              : []),
            { title: t("Edit Row…"), icon: "edit" as const, action: () => onOpenRow(row) },
            { title: rows.length === 1 ? t("Delete Row…") : t("Delete %@ Rows…", rows.length), icon: "trash" as const, destructive: true, action: () => confirmDelete(rows) },
          ]
        : []),
    ]);
  };

  const confirmDelete = (rows: number[]) => confirmDeleteRows(model, rows);

  const status = (() => {
    if (!isTable) return model.runSummary;
    const searching = model.tableSearch.trim() !== "";
    if (result.rows.length === 0) return searching ? t("No rows match") : result.note ?? t("Empty table");
    const first = model.offset + 1;
    const last = model.offset + result.rows.length;
    const total =
      typeof model.tableCount === "number"
        ? formatNumber(model.tableCount)
        : !searching && model.relation && model.relation.estimate >= 0
          ? `~${formatNumber(model.relation.estimate)}`
          : "?";
    const rows = t("Rows %@–%@ of %@", formatNumber(first), formatNumber(last), total);
    return result.note ? `${rows} · ${result.note}` : rows;
  })();

  const exportAs = async (format: "csv" | "json") => {
    try {
      const text = await api.exportResult(shown, format);
      await copyText(text);
      model.showToast(format === "csv" ? t("Copied as CSV") : t("Copied as JSON"));
    } catch (e) {
      model.showToast(errorText(e));
    }
  };

  return (
    <div className="result">
      <ResultGrid
        result={shown}
        version={isTable ? model.tableVersion : model.runVersion}
        startIndex={isTable ? model.offset : 0}
        sort={isTable ? model.tableSort : model.querySort}
        onSort={onSort}
        selection={model.selection}
        onSelectionChange={onSelectionChange}
        onEditCell={editable ? onEditCell : undefined}
        onOpenRow={editable ? onOpenRow : undefined}
        onContextMenu={onContextMenu}
        onCopied={onCopied}
      />
      <footer className="result-footer">
        {isTable && (
          <div className="control-group">
            <button className="button icon-only" disabled={!model.canGoBack} onClick={() => void model.previousPage()} title={withKeys(t("Previous page"), "previousPage")} aria-label={t("Previous")}>
              <Icon name="chevronLeft" size={14} />
            </button>
            <button className="button icon-only" disabled={!model.canGoForward} onClick={() => void model.nextPage()} title={withKeys(t("Next page"), "nextPage")} aria-label={t("Next")}>
              <Icon name="chevronRight" size={14} />
            </button>
          </div>
        )}
        <span className="status">{status}</span>
        {isTable && (
          <select
            className="button compact"
            value={model.pageSize}
            title={t("How many rows each page shows")}
            onChange={(e) => void model.setPageSize(Number(e.target.value))}
          >
            {pageSizes.map((size) => (
              <option key={size} value={size}>
                {t("%@ per page", formatNumber(size))}
              </option>
            ))}
          </select>
        )}
        {isTable && model.tableLoading && <span className="spinner small" />}
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
            title={withKeys(t("Copy all rows on screen, with the header, to paste into a spreadsheet"), "copyRows")}
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
