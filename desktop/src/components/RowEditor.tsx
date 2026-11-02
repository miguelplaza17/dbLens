// One row in a sheet: every column with its type, a NULL button each, and
// Save (only the changed columns are sent), Add for a new row, or Delete
// (RowEditor.swift).

import { useMemo, useState } from "react";
import { errorText, type Cell, type CellValue } from "../api";
import { t } from "../i18n";
import { useModel } from "../model";
import { Icon } from "./Icon";
import { Sheet } from "./Sheet";
import { confirmDeleteRows } from "./ResultPane";

interface Field {
  value: string;
  isNull: boolean;
  /** New rows: false until something is typed, so the column gets its default. */
  touched: boolean;
}

export function RowEditor({ row }: { row: number | null }) {
  const model = useModel();
  const result = model.tableResult;
  const isNew = row === null;
  // The row's cells from the grid's block cache (any row of the table, not just the first block).
  const original = useMemo(() => (row === null ? null : model.tableRow(row)), [row, model]);
  const [fields, setFields] = useState<Field[]>(() =>
    (result?.columns ?? []).map((_, i) => {
      const value = original?.[i] ?? null;
      return { value: value ?? "", isNull: !isNew && value === null, touched: false };
    }),
  );
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  if (!result || !model.relation) return null;
  const columns = result.columns;

  const value = (f: Field): Cell => (f.isNull ? null : f.value);
  const changes: CellValue[] = columns
    .map((c, i) => ({ column: c.name, value: value(fields[i]), i }))
    .filter(({ value: v, i }) => (isNew ? fields[i].touched : v !== (original?.[i] ?? null)))
    .map(({ column, value: v }) => ({ column, value: v }));

  const set = (i: number, next: Partial<Field>) => setFields(fields.map((f, j) => (j === i ? { ...f, ...next, touched: true } : f)));

  const save = async () => {
    setSaving(true);
    setError(null);
    try {
      if (isNew) await model.insertRow(changes);
      else await model.updateRow(row, changes);
      model.open(null);
    } catch (e) {
      setError(errorText(e));
    }
    setSaving(false);
  };

  const title = isNew
    ? t("New row in %@", model.relation.name)
    : t("Row %lld of %@", model.offset + row + 1, model.relation.name);

  return (
    <Sheet label={title} width={640} onClose={() => !saving && model.open(null)}>
      <form
        className="sheet-body"
        onSubmit={(e) => {
          e.preventDefault();
          void save();
        }}
      >
        <h2 className="sheet-title">{title}</h2>
        <div className="form-card row-fields">
          {columns.map((column, i) => (
            <div className="row-field" key={`${column.name}-${i}`}>
              <div className="row-field-label">
                <span className="row-field-name">{column.name}</span>
                <span className="row-field-type">{column.type}</span>
              </div>
              <textarea
                className="mono"
                rows={Math.min(6, Math.max(1, Math.ceil(fields[i].value.length / 56), fields[i].value.split("\n").length))}
                value={fields[i].isNull ? "" : fields[i].value}
                placeholder={fields[i].isNull ? "NULL" : isNew ? t("default") : ""}
                spellCheck={false}
                autoFocus={i === 0}
                onChange={(e) => set(i, { value: e.target.value, isNull: false })}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
                    e.preventDefault();
                    void save();
                  }
                }}
              />
              <button
                type="button"
                className={`button compact null-button${fields[i].isNull ? " on" : ""}`}
                title={t("Write NULL to this column")}
                aria-pressed={fields[i].isNull}
                onClick={() => set(i, { isNull: !fields[i].isNull })}
              >
                NULL
              </button>
            </div>
          ))}
        </div>
        <p className="form-footer">{isNew ? t("Empty fields get the column's default value.") : t("Only the changed columns are sent.")}</p>
        {error && (
          <div className="form-error" role="alert">
            <Icon name="alert" size={14} />
            <span className="selectable">{error}</span>
          </div>
        )}
        <footer className="sheet-actions">
          {!isNew && model.canEditRows && (
            <button type="button" className="button destructive-text" onClick={() => confirmDeleteRows(model, [row])}>
              {t("Delete Row…")}
            </button>
          )}
          <span className="spacer" />
          <button type="button" className="button" onClick={() => model.open(null)} disabled={saving}>
            {t("Cancel")}
          </button>
          <button type="submit" className="button prominent" disabled={saving || (!isNew && changes.length === 0)}>
            {isNew ? t("Add") : t("Save")}
          </button>
        </footer>
      </form>
    </Sheet>
  );
}
