// Where a dragged sidebar row would land, from the pointer's place on the rows
// (no DOM here, so it is tested on its own: tests/dragSpots.test.ts). It follows
// the Mac app's outline view: between two rows the nearer edge wins, the middle
// of a folder row means "inside it", and a folder is put in order at its own
// level by the top and bottom quarter of the folder row under it.
//
// The rows are measured once, at rest (no gap open), and the pointer is mapped
// onto that layout. The gap that opens under a drag moves the rows below it, and
// reading the moved rows would flip the spot back and forth.

/** A row as it sits at rest, in the list's own coordinates (top and bottom edges). */
export type SnapRow =
  | { kind: "connection"; id: string; folder: string; top: number; bottom: number }
  | { kind: "folder"; id: string; parent: string; open: boolean; top: number; bottom: number };

export interface Snapshot {
  /** The "Connections" header: a drop on it means the top level. */
  header: { top: number; bottom: number } | null;
  /** The visible rows, top to bottom. */
  rows: SnapRow[];
}

/** Inside `folder` ("" is the top level), in front of folder `before` or connection `row` when set, else last. */
export interface DropSpot {
  folder: string;
  before: string | null;
  row: string | null;
}

export interface Target {
  spot: DropSpot;
  /** The folder row that lights up for a drop inside it ("" is the header); none for a drop between rows. */
  into: string | null;
}

/** How much of a folder row's top (and bottom) means "in front of" (and "after") it. */
const EDGE = 0.25;

const endOfTopLevel: Target = { spot: { folder: "", before: null, row: null }, into: null };

/**
 * The target under `y`. `dragsFolder`: the dragged row is a folder. `allowed`
 * refuses spots this drag can't take (itself, its own subfolders): the answer is then null.
 */
export function resolveTarget(
  snapshot: Snapshot,
  y: number,
  dragsFolder: boolean,
  allowed: (spot: DropSpot) => boolean = () => true,
): Target | null {
  const target = resolve(snapshot, y, dragsFolder);
  return target && allowed(target.spot) ? target : null;
}

function resolve(snapshot: Snapshot, y: number, dragsFolder: boolean): Target | null {
  const { header, rows } = snapshot;
  if (header && y < header.bottom) return { spot: { folder: "", before: null, row: null }, into: "" };
  const at = rows.findIndex((r) => y >= r.top && y < r.bottom);
  if (at < 0) return endOfTopLevel;
  const row = rows[at];
  const fraction = (y - row.top) / Math.max(1, row.bottom - row.top);

  if (row.kind === "connection") {
    if (dragsFolder) {
      // A folder can't go among connections: the start of the subfolders of that folder.
      const first = rows.find((r) => r.kind === "folder" && r.parent === row.folder);
      return { spot: { folder: row.folder, before: first?.id ?? null, row: null }, into: null };
    }
    if (fraction < 0.5) return { spot: { folder: row.folder, before: null, row: row.id }, into: null };
    return { spot: { folder: row.folder, before: null, row: nextConnection(rows, at)?.id ?? null }, into: null };
  }

  // A folder row.
  if (!dragsFolder) {
    // Connections go before the folders: the top edge means the end of this level's connections.
    if (fraction < EDGE) return { spot: { folder: row.parent, before: null, row: null }, into: null };
    return inside(row);
  }
  if (fraction < EDGE) return { spot: { folder: row.parent, before: row.id, row: null }, into: null };
  // After an open folder would be far below the pointer (after its contents): the bottom of an open one means inside.
  if (fraction >= 1 - EDGE && !row.open) {
    return { spot: { folder: row.parent, before: nextFolder(rows, at)?.id ?? null, row: null }, into: null };
  }
  return inside(row);
}

function inside(row: Extract<SnapRow, { kind: "folder" }>): Target {
  return { spot: { folder: row.id, before: null, row: null }, into: row.id };
}

/** The next connection in the same folder after row `at`. */
function nextConnection(rows: SnapRow[], at: number): Extract<SnapRow, { kind: "connection" }> | undefined {
  const row = rows[at];
  if (row.kind !== "connection") return undefined;
  for (let i = at + 1; i < rows.length; i++) {
    const next = rows[i];
    if (next.kind === "connection" && next.folder === row.folder) return next;
  }
  return undefined;
}

/** The next folder at the same level after row `at`. */
function nextFolder(rows: SnapRow[], at: number): Extract<SnapRow, { kind: "folder" }> | undefined {
  const row = rows[at];
  if (row.kind !== "folder") return undefined;
  for (let i = at + 1; i < rows.length; i++) {
    const next = rows[i];
    if (next.kind === "folder" && next.parent === row.parent) return next;
  }
  return undefined;
}

/**
 * Maps a pointer position onto the layout without the gap: below the open gap
 * the rows have moved down by its height, and inside it the pointer is on its top edge.
 * `gap` is where the gap is now (its top, and its height with margins), or null.
 */
export function restY(y: number, gap: { top: number; height: number } | null): number {
  if (!gap) return y;
  if (y < gap.top) return y;
  if (y < gap.top + gap.height) return gap.top;
  return y - gap.height;
}
