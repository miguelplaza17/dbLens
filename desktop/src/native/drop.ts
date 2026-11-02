// What a drop means. File drops come from the shell (paths); text drops are
// connection strings pasted from somewhere else.

const databaseExtensions = [".db", ".sqlite", ".sqlite3", ".db3"];

export function isDatabasePath(path: string): boolean {
  const lower = path.toLowerCase();
  return databaseExtensions.some((e) => lower.endsWith(e));
}

/** The first dropped path that is a SQLite file, or null. */
export function firstDatabase(paths: string[]): string | null {
  return paths.find(isDatabasePath) ?? null;
}

/** The text of a dropped connection string: one line, no spaces in a URL. null for anything else. */
export function connectionText(text: string): string | null {
  const trimmed = text.trim();
  if (!trimmed || trimmed.length > 2000 || trimmed.includes("\n")) return null;
  return /^[a-z][a-z0-9+.-]*:\/\/\S+$/i.test(trimmed) || /^(sqlite3?|file):\S+$/i.test(trimmed) ? trimmed : null;
}

/** The text to connect to a SQLite file: the path itself. The backend reads a
 *  plain path (Engine::detect), so spaces, # and % need no escaping. */
export function sqliteUrl(path: string): string {
  return path;
}
