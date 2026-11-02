// One window: sidebar | grid | foldable SQL panel (ContentView.swift).
// State and actions live in model.ts; this file lays out the window, applies
// the theme, and maps keyboard shortcuts (shortcuts.ts) to actions.

import { useEffect, useLayoutEffect, useRef } from "react";
import { engineNames } from "./api";
import { t } from "./i18n";
import { useModel, type AppModel, type Sheet as SheetState } from "./model";
import { actionFor, shortcutList, withKeys, type Action } from "./shortcuts";
import { dispatch, setPerformer } from "./native/actions";
import { installNative } from "./native/bridge";
import { performNative } from "./native/perform";
import { gridAction } from "./grid/commands";
import { PerfHud } from "./grid/PerfHud";
import { tsv, copyText } from "./format";
import { ConnectionSheet } from "./components/ConnectionSheet";
import { Icon } from "./components/Icon";
import { ResultPane, confirmDeleteRows } from "./components/ResultPane";
import { RowEditor } from "./components/RowEditor";
import { ContextMenu, Sheet } from "./components/Sheet";
import { Sidebar } from "./components/Sidebar";
import { SQLPanel } from "./components/SQLPanel";

export default function App() {
  const model = useModel();
  const filterRef = useRef<HTMLInputElement>(null);
  const searchRef = useRef<HTMLInputElement>(null);

  useEffect(() => void model.loadStore(), [model]);

  useLayoutEffect(() => {
    document.documentElement.dataset.theme = model.theme;
  }, [model.theme]);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      const target = event.target as HTMLElement | null;
      const typing = !!target && (target.isContentEditable || ["INPUT", "TEXTAREA", "SELECT"].includes(target.tagName));
      if (model.sheet && event.key !== "Escape") return;
      const action = actionFor(event, typing);
      if (!action) return;
      // The editor handles its own run keys (so a selection runs).
      if (target?.closest(".cm-editor") && ["run", "rehearse", "runAtomically"].includes(action)) return;
      event.preventDefault();
      dispatch(action);
    };
    // Keys and native menu clicks end up here: the same code runs for both.
    setPerformer((action) => perform(model, action, { filter: filterRef.current, search: searchRef.current }));
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("keydown", onKey);
      setPerformer(null);
    };
  }, [model]);

  useEffect(() => installNative(model), [model]);

  const title = model.session?.name ?? "dbLens";
  const subtitle = !model.session
    ? model.connecting
      ? t("Connecting…")
      : t("No Database Open")
    : model.shown === "query"
      ? t("Query Result")
      : model.relation
        ? model.relation.kind === "key" || !model.relation.schema
          ? model.relation.name
          : `${model.relation.schema}.${model.relation.name}`
        : `${engineNames[model.session.engine]} ${model.session.serverVersion}`;

  return (
    <div className={`window${model.showSidebar ? "" : " no-sidebar"}`}>
      {model.showSidebar && <Sidebar ref={filterRef} />}
      <main className="main">
        <header className="toolbar">
          <button
            className="icon-button"
            onClick={() => model.toggleSidebar()}
            title={withKeys(t("Show or Hide the Sidebar"), "toggleSidebar")}
            aria-label={t("Show or Hide the Sidebar")}
          >
            <Icon name="sidebar" />
          </button>
          <div className="toolbar-title">
            <span className="title">{title}</span>
            <span className="subtitle">{subtitle}</span>
          </div>
          {model.session && model.execution && model.relation && (
            <div className="segmented small" role="tablist" aria-label={t("Shown")}>
              <button role="tab" aria-selected={model.shown === "table"} className={model.shown === "table" ? "selected" : ""} onClick={() => model.showResultTab("table")}>
                {t("Table")}
              </button>
              <button role="tab" aria-selected={model.shown === "query"} className={model.shown === "query" ? "selected" : ""} onClick={() => model.showResultTab("query")}>
                {t("Query Result")}
              </button>
            </div>
          )}
          <span className="spacer" />
          {model.session && (
            <>
              <button className="icon-button" onClick={() => void model.reload()} title={withKeys(t("Reload"), "refresh")} aria-label={t("Reload")}>
                <Icon name="refresh" />
              </button>
              <button className="icon-button" onClick={() => void model.disconnect()} title={withKeys(t("Disconnect"), "disconnect")} aria-label={t("Disconnect")}>
                <Icon name="disconnect" />
              </button>
            </>
          )}
          {!model.showSQLPanel && (
            <button
              className="icon-button"
              onClick={() => model.toggleSQLPanel()}
              title={withKeys(t("Show SQL Panel"), "toggleSQLPanel")}
              aria-label={t("Show SQL Panel")}
            >
              <Icon name="panelBottom" />
            </button>
          )}
          <button className="icon-button" onClick={() => model.open({ kind: "shortcuts" })} title={t("Keyboard Shortcuts")} aria-label={t("Keyboard Shortcuts")}>
            <Icon name="keyboard" />
          </button>
          <button
            className="icon-button"
            onClick={() => model.toggleTheme()}
            title={withKeys(model.theme === "dark" ? t("Light") : t("Dark"), "toggleTheme")}
            aria-label={t("Light or Dark")}
          >
            <Icon name={model.theme === "dark" ? "sun" : "moon"} />
          </button>
        </header>
        <div className="content">
          <ResultPane ref={searchRef} />
          <PerfHud />
        </div>
        {model.showSQLPanel && (
          <>
            <Splitter />
            <div className="bottom" style={{ height: model.sqlPanelHeight }}>
              <SQLPanel />
            </div>
          </>
        )}
      </main>
      {model.sheet && <SheetView sheet={model.sheet} />}
      {model.menu && <ContextMenu menu={model.menu} onClose={model.closeMenu} />}
      {model.toast && (
        <div className="toast" role="status">
          {model.toast}
        </div>
      )}
    </div>
  );
}

/** Drag to change the SQL panel's height. */
function Splitter() {
  const model = useModel();
  const start = (event: React.MouseEvent) => {
    event.preventDefault();
    const startY = event.clientY;
    const startHeight = model.sqlPanelHeight;
    const move = (e: MouseEvent) => {
      const max = window.innerHeight - 200;
      model.setSQLPanelHeight(Math.min(max, Math.max(140, startHeight - (e.clientY - startY))));
    };
    const up = () => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
      document.body.classList.remove("resizing-rows");
    };
    document.body.classList.add("resizing-rows");
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
  };
  return (
    <div className="splitter" role="separator" aria-orientation="horizontal" onMouseDown={start} onDoubleClick={() => model.toggleSQLPanel()}>
      <span className="splitter-grip" />
    </div>
  );
}

function SheetView({ sheet }: { sheet: SheetState }) {
  const model = useModel();
  switch (sheet.kind) {
    case "connection":
      return <ConnectionSheet editing={sheet.editing} initialUrl={sheet.url} />;
    case "row":
      return <RowEditor row={sheet.row} />;
    case "confirm":
      return (
        <Sheet label={sheet.title} width={400} onClose={() => model.open(null)}>
          <div className="sheet-body confirm">
            <h2 className="sheet-title">{sheet.title}</h2>
            <p className="secondary">{sheet.message}</p>
            <footer className="sheet-actions">
              <span className="spacer" />
              <button className="button" onClick={() => model.open(null)}>
                {t("Cancel")}
              </button>
              <button
                className={`button prominent${sheet.destructive ? " danger" : ""}`}
                autoFocus
                onClick={async () => {
                  model.open(null);
                  await sheet.run();
                }}
              >
                {sheet.action}
              </button>
            </footer>
          </div>
        </Sheet>
      );
    case "shortcuts":
      return (
        <Sheet label={t("Keyboard Shortcuts")} width={440} onClose={() => model.open(null)}>
          <div className="sheet-body">
            <h2 className="sheet-title">{t("Keyboard Shortcuts")}</h2>
            <div className="form-card shortcuts">
              {shortcutList().map((s) => (
                <div className="shortcut-row" key={s.title}>
                  <span>{s.title}</span>
                  <kbd>{s.keys}</kbd>
                </div>
              ))}
            </div>
            <footer className="sheet-actions">
              <span className="spacer" />
              <button className="button prominent" autoFocus onClick={() => model.open(null)}>
                {t("Done")}
              </button>
            </footer>
          </div>
        </Sheet>
      );
  }
}

/** What each shortcut does. */
function perform(model: AppModel, action: Action, fields: { filter: HTMLInputElement | null; search: HTMLInputElement | null }) {
  // The grid on screen takes copy, paging and the perf HUD first.
  if (gridAction(action)) return;
  if (performNative(model, action)) return;
  switch (action) {
    case "newConnection":
      return model.open({ kind: "connection", editing: null });
    case "disconnect":
      return void model.disconnect();
    case "refresh":
      return void model.reload();
    case "toggleSidebar":
      return model.toggleSidebar();
    case "toggleSQLPanel":
      return model.toggleSQLPanel();
    case "filterTables":
      if (!model.showSidebar) model.toggleSidebar();
      return requestAnimationFrame(() => fields.filter?.focus());
    case "searchRows":
      return fields.search?.focus();
    case "previousPage":
      return void model.previousPage();
    case "nextPage":
      return void model.nextPage();
    case "newRow":
      return model.canInsertRows && model.open({ kind: "row", row: null });
    case "deleteRows":
      return confirmDeleteRows(model, [...model.selection]);
    case "copyRows": {
      const result = model.shownResult;
      if (!result) return;
      return void copyText(tsv(result)).then(() => model.showToast(t("Copied %@ rows", result.rowCount)));
    }
    case "run":
      return void model.run();
    case "rehearse":
      return void model.run("rehearse");
    case "runAtomically":
      return void model.run("atomic");
    case "cancel":
      return model.cancel();
    case "toggleTheme":
      return model.toggleTheme();
  }
}
