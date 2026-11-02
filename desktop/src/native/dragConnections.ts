// Drag a saved connection onto a folder. Pointer events, not HTML5 drag and
// drop: on Windows Tauri's file-drop handler takes over the web view's own
// drag and drop, and this works the same everywhere.
//
// A folder row (or the "Connections" header, for no folder) carries
// data-drop-folder="<id>". Mouse down on a connection row starts a drag once
// the pointer has moved a few pixels; a click that was a drag is swallowed.

const THRESHOLD = 5;
export const DROP_CLASS = "drop-target";

/** The folder id under a point, "" for the top level, or null for nothing. */
export function folderAt(x: number, y: number): string | null {
  const el = document.elementFromPoint(x, y)?.closest<HTMLElement>("[data-drop-folder]");
  return el ? (el.dataset.dropFolder ?? "") : null;
}

export function beginConnectionDrag(event: React.MouseEvent, onDrop: (folderId: string) => void) {
  if (event.button !== 0) return;
  const startX = event.clientX;
  const startY = event.clientY;
  let dragging = false;
  let hovered: HTMLElement | null = null;
  const mark = (x: number, y: number) => {
    const el = document.elementFromPoint(x, y)?.closest<HTMLElement>("[data-drop-folder]") ?? null;
    if (el === hovered) return;
    hovered?.classList.remove(DROP_CLASS);
    hovered = el;
    hovered?.classList.add(DROP_CLASS);
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
    hovered?.classList.remove(DROP_CLASS);
    document.body.classList.remove("dragging-connection");
    if (!dragging) return;
    // The click that ends a drag must not connect.
    window.addEventListener("click", (c) => c.stopPropagation(), { capture: true, once: true });
    const folder = folderAt(e.clientX, e.clientY);
    if (folder !== null) onDrop(folder);
  };
  window.addEventListener("mousemove", move);
  window.addEventListener("mouseup", finish);
}
