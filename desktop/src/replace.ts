// Find and replace in table rows: which cells of a row change, and to what
// (RowReplace.swift on the Mac). Only text columns, and never the columns that
// identify the row: a search also matches numbers and dates.

export interface ReplaceColumn {
  name: string;
  type: string;
}

/** "text", "varchar(20)", "character varying", "CLOB", "citext"… */
export const isTextType = (type: string) => /char|text|clob/i.test(type);

/** Every occurrence, ignoring case like the search; both strings are plain text. */
export function replaceText(value: string, find: string, replacement: string): string {
  const pattern = new RegExp(find.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"), "gi");
  return value.replace(pattern, () => replacement);
}

/** The columns to write for this row, with their new values. */
export function rowChanges(
  columns: ReplaceColumn[],
  row: Array<string | null>,
  keyColumns: string[],
  find: string,
  replacement: string,
): Array<{ column: string; value: string }> {
  if (find === "") return [];
  const changes: Array<{ column: string; value: string }> = [];
  columns.forEach((column, i) => {
    const value = row[i];
    if (value === null || value === undefined || !isTextType(column.type) || keyColumns.includes(column.name)) return;
    const next = replaceText(value, find, replacement);
    if (next !== value) changes.push({ column: column.name, value: next });
  });
  return changes;
}
