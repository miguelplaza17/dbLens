// The panel at the bottom: the editor with its actions, and the log of the
// last run (SQLPanel.swift). Mod-Enter runs the selection, or everything.
// Redis and MongoDB switch it to commands: no rehearsal, no all-or-nothing.

import { useCallback } from "react";
import { t } from "../i18n";
import { useModel } from "../model";
import { copyText } from "../format";
import { keyLabel, withKeys } from "../shortcuts";
import { Icon } from "./Icon";
import { SQLEditor } from "./SQLEditor";

export function SQLPanel() {
  const model = useModel();
  const commands = model.usesCommands || model.usesQueries;
  const connected = !!model.session;

  const onChange = useCallback((text: string) => model.setSQL(text), [model]);
  const onSelection = useCallback(
    (selected: string) => {
      if (selected !== model.sqlSelection) {
        model.sqlSelection = selected;
        model.changed();
      }
    },
    [model],
  );

  const placeholder = model.usesCommands
    ? "GET user:42"
    : model.usesQueries
      ? "db.users.find({ active: true })"
      : connected
        ? "select * from …"
        : t("Connect to a database to run SQL.");

  return (
    <div className="sql-panel">
      <section className="card editor-card">
        <header className="card-header">
          <button className="icon-button" onClick={() => model.toggleSQLPanel()} title={withKeys(t("Hide SQL Panel"), "toggleSQLPanel")} aria-label={t("Hide SQL Panel")}>
            <Icon name="panelBottom" size={14} />
          </button>
          <span className="card-title">{model.panelTitle}</span>
          {model.running ? (
            <>
              <span className="spinner small" />
              <span className="secondary">{t("Running…")}</span>
            </>
          ) : (
            <span className="secondary hint">
              {model.sqlSelection.trim() ? t("%@ runs the selection", keyLabel("run")) : t("%@ runs everything", keyLabel("run"))}
            </span>
          )}
          <span className="spacer" />
          {!commands && (
            <>
              <button
                className="button"
                disabled={!connected || model.running}
                onClick={() => void model.run("rehearse")}
                title={withKeys(t("Run in a transaction that is always rolled back: see what it would do, change nothing"), "rehearse")}
              >
                <Icon name="rehearse" size={14} />
                {t("Rehearse")}
              </button>
              <button
                className="button"
                disabled={!connected || model.running}
                onClick={() => void model.run("atomic")}
                title={withKeys(t("Run every statement in one transaction: all of them, or none"), "runAtomically")}
              >
                <Icon name="atomic" size={14} />
                {t("All or Nothing")}
              </button>
            </>
          )}
          {model.running ? (
            <button className="button" onClick={() => model.cancel()} title={withKeys(t("Cancel"), "cancel")}>
              <Icon name="stop" size={12} />
              {t("Cancel")}
            </button>
          ) : (
            <button className="button prominent" disabled={!connected} onClick={() => void model.run()} title={withKeys(t("Run"), "run")}>
              <Icon name="play" size={12} />
              {t("Run")}
            </button>
          )}
        </header>
        <SQLEditor
          value={model.sql}
          onChange={onChange}
          onSelectionChange={onSelection}
          engine={model.engine}
          relations={model.relations}
          placeholder={placeholder}
          onRun={() => void model.run()}
          onRehearse={() => void model.run("rehearse")}
          onRunAtomically={() => void model.run("atomic")}
        />
      </section>
      <LogCard />
    </div>
  );
}

function LogCard() {
  const model = useModel();
  const entry = model.log;
  const copy = () => entry && void copyText([entry.message, entry.sql].filter(Boolean).join("\n")).then(() => model.showToast(t("Copied")));
  return (
    <section className="card log-card" aria-label={t("Log")}>
      <header className="card-header">
        <span className="card-title">{t("Log")}</span>
        <span className="spacer" />
        <button className="icon-button" disabled={!entry} onClick={copy} title={t("Copy the log (message and SQL)")} aria-label={t("Copy Log")}>
          <Icon name="copy" size={14} />
        </button>
      </header>
      <div className="log-body">
        {entry ? (
          <div className="log-entry">
            <div className="log-line">
              <span className="tertiary">{entry.date.toLocaleTimeString()}</span>
              <span className={entry.isError ? "log-icon error" : "log-icon ok"}>
                <Icon name={entry.isError ? "xCircle" : "checkCircle"} size={12} />
              </span>
              <span className={`selectable${entry.isError ? " error-text" : ""}`}>{entry.message}</span>
            </div>
            {entry.sql && <pre className="log-sql selectable">{entry.sql}</pre>}
            {entry.runAnyway && (
              <button className="button" onClick={() => void model.run("plain", entry.runAnyway)}>
                {t("Run Statement by Statement")}
              </button>
            )}
          </div>
        ) : (
          <p className="log-empty">{t("The result of the last run appears here.")}</p>
        )}
      </div>
    </section>
  );
}
