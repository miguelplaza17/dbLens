// The accent colors, as Theme.all in Sources/dbLens/UI/Theme.swift. The ids
// match src-tauri/src/settings.rs (ACCENTS); the colors are in accents.css,
// which is part of the theme (no color is written anywhere else).

export type AccentId =
  | "system"
  | "elephant"
  | "blue"
  | "indigo"
  | "violet"
  | "pink"
  | "red"
  | "orange"
  | "yellow"
  | "mint"
  | "teal"
  | "graphite";

/** id and English name (shown through t()). "system" keeps the theme's own accent. */
export const ACCENTS: { id: AccentId; name: string }[] = [
  { id: "system", name: "System" },
  { id: "elephant", name: "Postgres" },
  { id: "blue", name: "Blue" },
  { id: "indigo", name: "Indigo" },
  { id: "violet", name: "Violet" },
  { id: "pink", name: "Pink" },
  { id: "red", name: "Red" },
  { id: "orange", name: "Orange" },
  { id: "yellow", name: "Yellow" },
  { id: "mint", name: "Mint" },
  { id: "teal", name: "Teal" },
  { id: "graphite", name: "Graphite" },
];

/** Puts the accent on <html> as class `accent-<id>` (the grid watches the class attribute). */
export function applyAccent(id: AccentId) {
  const root = document.documentElement;
  for (const name of [...root.classList]) if (name.startsWith("accent-")) root.classList.remove(name);
  if (id !== "system") root.classList.add(`accent-${id}`);
}
