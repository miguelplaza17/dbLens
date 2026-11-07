// Drag a saved connection or a folder in the sidebar. Pointer events, not
// HTML5 drag and drop: on Windows Tauri's file-drop handler takes over the web
// view's own drag and drop, and this works the same everywhere.
//
// A folder row (or the "Connections" header, for no folder) carries
// data-drop-folder="<id>". A folder row also carries data-folder-parent: when
// a folder is dragged onto the top edge of another folder's row, it lands in
// front of that folder (in its parent) instead of inside it. Mouse down on a
// row starts a drag once the pointer has moved a few pixels; a click that was
// a drag is swallowed.

const THRESHOLD = 5;
export const DROP_CLASS = "drop-target";
export const DROP_BEFORE_CLASS = "drop-before";

/** Where a drag ends: inside `folder` ("" is the top level), in front of folder `before` when set. */
export interface DropSpot {
  folder: string;
  before: string | null;
}

interface Hit {
  el: HTMLElement;
  spot: DropSpot;
}

/** The drop spot under a point. `reorder`: a folder is dragged, so a folder row's top edge means "in front". */
function hitAt(x: number, y: number, reorder: boolean): Hit | null {
  const el = document.elementFromPoint(x, y)?.closest<HTMLElement>("[data-drop-folder]");
  if (!el) return null;
  const folder = el.dataset.dropFolder ?? "";
  const parent = el.dataset.folderParent;
  if (reorder && folder && parent !== undefined) {
    const box = el.getBoundingClientRect();
    if (y < box.top + box.height / 3) return { el, spot: { folder: parent, before: folder } };
  }
  return { el, spot: { folder, before: null } };
}

/** The folder id under a point, "" for the top level, or null for nothing. */
export function folderAt(x: number, y: number): string | null {
  return hitAt(x, y, false)?.spot.folder ?? null;
}

export function beginConnectionDrag(event: React.MouseEvent, onDrop: (folderId: string) => void) {
  beginSidebarDrag(event, false, (spot) => onDrop(spot.folder));
}

/** A folder drags like a connection, and can also go in front of another folder. `allowed` skips spots it can't take. */
export function beginFolderDrag(event: React.MouseEvent, allowed: (spot: DropSpot) => boolean, onDrop: (spot: DropSpot) => void) {
  beginSidebarDrag(event, true, (spot) => allowed(spot) && onDrop(spot), allowed);
}

function beginSidebarDrag(
  event: React.MouseEvent,
  reorder: boolean,
  onDrop: (spot: DropSpot) => void,
  allowed: (spot: DropSpot) => boolean = () => true,
) {
  if (event.button !== 0) return;
  const startX = event.clientX;
  const startY = event.clientY;
  let dragging = false;
  let hovered: HTMLElement | null = null;
  let hoveredClass = DROP_CLASS;
  const clear = () => hovered?.classList.remove(hoveredClass);
  const mark = (x: number, y: number) => {
    const hit = hitAt(x, y, reorder);
    const el = hit && allowed(hit.spot) ? hit.el : null;
    const cls = hit?.spot.before ? DROP_BEFORE_CLASS : DROP_CLASS;
    if (el === hovered && cls === hoveredClass) return;
    clear();
    hovered = el;
    hoveredClass = cls;
    hovered?.classList.add(hoveredClass);
  };
  const move = (e: MouseEvent) => {
    if (!dragging && Math.hypot(e.clientX - startX, e.clientY - startY) < THRESHOLD) return;
    if (!dragging) {
      dragging = true;
      document.body.classList.add("dragging-connection");
    }
    mark(e.clientX, e.clientY);
  };
  const finish = (e: MouseEvent) => {
    window.removeEventListener("mousemove", move);
    window.removeEventListener("mouseup", finish);
    clear();
    document.body.classList.remove("dragging-connection");
    if (!dragging) return;
    // The click that ends a drag must not connect (or fold the folder).
    window.addEventListener("click", (c) => c.stopPropagation(), { capture: true, once: true });
    const hit = hitAt(e.clientX, e.clientY, reorder);
    if (hit) onDrop(hit.spot);
  };
  window.addEventListener("mousemove", move);
  window.addEventListener("mouseup", finish);
}
