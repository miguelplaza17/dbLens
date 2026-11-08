// Drag a saved connection or a folder in the sidebar. Pointer events, not
// HTML5 drag and drop: on Windows Tauri's file-drop handler takes over the web
// view's own drag and drop, and this works the same everywhere.
//
// Rows say what they are: a folder row (or the "Connections" header, for no
// folder) carries data-drop-folder="<id>", a connection row data-drop-row="<id>"
// and data-row-folder. Empty .drop-slot elements sit where a row can land; the
// one for the current spot opens, so the rows below move down to make room.
// Mouse down on a row starts a drag once the pointer has moved a few pixels; a
// click that was a drag is swallowed.

const THRESHOLD = 5;
/** How long a closed folder waits under a drag before it opens (Motion.springLoad on the Mac). */
const SPRING_MS = 600;
const DROP_CLASS = "drop-target";
const SOURCE_CLASS = "dragging-source";

/**
 * Where a drag ends: inside `folder` ("" is the top level), in front of folder
 * `before` or connection `row` when set, else last there.
 */
export interface DropSpot {
  folder: string;
  before: string | null;
  row: string | null;
}

interface Hit {
  /** The row that lights up: a folder or the header. */
  el: HTMLElement | null;
  spot: DropSpot;
}

/** The drop spot under a point, or null. `dragsFolder`: a folder's top edge means "in front". */
function hitAt(x: number, y: number, dragsFolder: boolean, last: Hit | null): Hit | null {
  const under = document.elementFromPoint(x, y);
  // On the open slot the rows have moved away from: still the same spot.
  if (under?.closest(".drop-slot.open")) return last;
  const row = under?.closest<HTMLElement>("[data-drop-row]");
  if (row) {
    const folder = row.dataset.rowFolder ?? "";
    return { el: null, spot: { folder, before: null, row: dragsFolder ? null : (row.dataset.dropRow ?? null) } };
  }
  const el = under?.closest<HTMLElement>("[data-drop-folder]");
  if (!el) return null;
  const folder = el.dataset.dropFolder ?? "";
  const parent = el.dataset.folderParent;
  if (dragsFolder && folder && parent !== undefined) {
    const box = el.getBoundingClientRect();
    if (y < box.top + box.height / 3) return { el: null, spot: { folder: parent, before: folder, row: null } };
  }
  return { el, spot: { folder, before: null, row: null } };
}

/** The empty slot that opens for a spot; a closed folder has none yet. */
function slotFor(spot: DropSpot, dragsFolder: boolean): HTMLElement | null {
  const key = spot.row ? `before:${spot.row}` : spot.before ? `before:${spot.before}` : `${dragsFolder ? "folders" : "connections"}:${spot.folder}`;
  return document.querySelector<HTMLElement>(`.drop-slot[data-slot="${CSS.escape(key)}"]`);
}

/** The card that follows the pointer: the dragged row's icon and name. */
function makeGhost(row: HTMLElement): HTMLElement {
  const ghost = document.createElement("div");
  ghost.className = "drag-ghost";
  const title = row.querySelector(".sidebar-row-title");
  const icon = title?.previousElementSibling;
  if (icon) ghost.append(icon.cloneNode(true));
  const name = document.createElement("span");
  name.textContent = title?.textContent ?? "";
  ghost.append(name);
  document.body.append(ghost);
  return ghost;
}

interface DragOptions {
  /** A spot this drag can't take shows nothing and drops nothing. */
  allowed: (spot: DropSpot) => boolean;
  onDrop: (spot: DropSpot) => void | Promise<unknown>;
  /** A closed folder under the drag opens after a moment (like Finder). */
  onSpring: (folder: string) => void;
}

export function beginConnectionDrag(event: React.MouseEvent, options: DragOptions) {
  beginSidebarDrag(event, false, options);
}

/** A folder drags like a connection, and can also go in front of another folder. */
export function beginFolderDrag(event: React.MouseEvent, options: DragOptions) {
  beginSidebarDrag(event, true, options);
}

const noShow = () => {};

function beginSidebarDrag(event: React.MouseEvent, dragsFolder: boolean, { allowed, onDrop, onSpring }: DragOptions) {
  if (event.button !== 0) return;
  const startX = event.clientX;
  const startY = event.clientY;
  const source = event.currentTarget as HTMLElement;
  let ghost: HTMLElement | null = null;
  let dragging = false;
  let last: Hit | null = null;
  let lit: HTMLElement | null = null;
  let slot: HTMLElement | null = null;
  let spotKey = "";
  let spring = 0;
  let pointer = { x: startX, y: startY };

  let show = (hit: Hit | null, force = false) => {
    const usable = hit && allowed(hit.spot) ? hit : null;
    const key = usable ? JSON.stringify(usable.spot) : "";
    if (key === spotKey && !force) return;
    if (key !== spotKey) {
      window.clearTimeout(spring);
      const folder = usable?.el ? usable.spot.folder : "";
      if (folder) {
        spring = window.setTimeout(() => {
          onSpring(folder);
          // The folder's slots exist once it has drawn open.
          window.setTimeout(() => show(last, true), 80);
        }, SPRING_MS);
      }
    }
    spotKey = key;
    lit?.classList.remove(DROP_CLASS);
    lit = usable?.el ?? null;
    lit?.classList.add(DROP_CLASS);
    const next = usable ? slotFor(usable.spot, dragsFolder) : null;
    if (next !== slot) {
      slot?.classList.remove("open");
      slot = next;
      slot?.classList.add("open");
    }
  };

  const move = (e: MouseEvent) => {
    if (!dragging && Math.hypot(e.clientX - startX, e.clientY - startY) < THRESHOLD) return;
    if (!dragging) {
      dragging = true;
      document.body.classList.add("dragging-connection");
      source.classList.add(SOURCE_CLASS);
      ghost = makeGhost(source);
    }
    pointer = { x: e.clientX, y: e.clientY };
    if (ghost) ghost.style.transform = `translate3d(${pointer.x + 12}px, ${pointer.y + 10}px, 0)`;
    last = hitAt(pointer.x, pointer.y, dragsFolder, last);
    show(last);
  };

  /** The row is in its place: the slot goes without animating, or the list would jump twice. */
  const closeSlot = (open: HTMLElement | null) => {
    if (!open) return;
    open.classList.add("instant");
    open.classList.remove("open");
    window.setTimeout(() => open.classList.remove("instant"), 50);
  };

  const finish = (e: MouseEvent) => {
    window.removeEventListener("mousemove", move);
    window.removeEventListener("mouseup", finish);
    window.clearTimeout(spring);
    show = noShow;
    lit?.classList.remove(DROP_CLASS);
    document.body.classList.remove("dragging-connection");
    source.classList.remove(SOURCE_CLASS);
    ghost?.remove();
    const open = slot;
    if (!dragging) return;
    // The click that ends a drag must not connect (or fold the folder).
    window.addEventListener("click", (c) => c.stopPropagation(), { capture: true, once: true });
    const hit = hitAt(e.clientX, e.clientY, dragsFolder, last);
    if (!hit || !allowed(hit.spot)) {
      open?.classList.remove("open");
      return;
    }
    void Promise.resolve(onDrop(hit.spot)).finally(() => window.setTimeout(() => closeSlot(open)));
  };
  window.addEventListener("mousemove", move);
  window.addEventListener("mouseup", finish);
}
