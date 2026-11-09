// Drag a saved connection or a folder in the sidebar. Pointer events, not
// HTML5 drag and drop: on Windows Tauri's file-drop handler takes over the web
// view's own drag and drop, and this works the same everywhere.
//
// It behaves like the Mac app's outline view (ContentView.swift, SidebarOutline.swift):
// the rows part to open a gap where the row will land (an empty `.drop-slot`
// opens, so the rows below move down), a folder lights up when the row will go
// inside it, a closed folder opens under a drag, the list scrolls near its
// edges, and the dragged card settles into its place when dropped.
//
// Where the row would land comes from dragSpots.ts, which reads the rows as they
// sit at rest: they are measured once, with no gap open, and the pointer is
// mapped onto that layout, so the gap moving the rows never flips the spot.
//
// Rows say what they are: a folder row (or the "Connections" header, for no
// folder) carries data-drop-folder="<id>" (and data-folder-parent, data-open), a
// connection row data-drop-row="<id>" and data-row-folder. Mouse down on a row
// starts a drag once the pointer has moved a few pixels; a click that was a drag
// is swallowed; Escape cancels.

import { resolveTarget, restY, type DropSpot, type SnapRow, type Snapshot, type Target } from "./dragSpots";

export type { DropSpot };

const THRESHOLD = 5;
/** How long a closed folder waits under a drag before it opens (Motion.springLoad on the Mac). */
const SPRING_MS = 600;
/** How near the list's edge (px) the pointer scrolls it, and the fastest it goes (px per frame). */
const SCROLL_ZONE = 32;
const SCROLL_MAX = 14;
/** The dragged card flying to its place, in ms. */
const SETTLE_MS = 200;
const DROP_CLASS = "drop-target";
const SOURCE_CLASS = "dragging-source";

const scroller = () => document.querySelector<HTMLElement>(".sidebar-scroll");
const headerEl = (box: HTMLElement) => box.querySelector<HTMLElement>('.sidebar-section-header[data-drop-folder=""]');

/** The rows as they sit at rest, in the list's own coordinates. */
function measure(box: HTMLElement): Snapshot {
  // Every gap shut (and open again at once, before anything is drawn).
  const open = [...box.querySelectorAll<HTMLElement>(".drop-slot.open")];
  for (const slot of open) {
    slot.classList.add("instant");
    slot.classList.remove("open");
  }
  const origin = box.getBoundingClientRect().top - box.scrollTop;
  const edges = (el: HTMLElement) => {
    const r = el.getBoundingClientRect();
    return { top: r.top - origin, bottom: r.bottom - origin };
  };
  const header = headerEl(box);
  const rows: SnapRow[] = [];
  for (const el of box.querySelectorAll<HTMLElement>("[data-drop-row], [data-drop-folder]")) {
    if (el === header) continue;
    if (el.dataset.dropRow !== undefined) {
      rows.push({ kind: "connection", id: el.dataset.dropRow, folder: el.dataset.rowFolder ?? "", ...edges(el) });
    } else {
      rows.push({
        kind: "folder",
        id: el.dataset.dropFolder ?? "",
        parent: el.dataset.folderParent ?? "",
        open: el.dataset.open === "true",
        ...edges(el),
      });
    }
  }
  const snapshot = { header: header ? edges(header) : null, rows };
  for (const slot of open) slot.classList.add("open");
  void box.offsetHeight;
  for (const slot of open) slot.classList.remove("instant");
  return snapshot;
}

/** The empty slot that opens for a spot (none for a drop inside a folder: that one lights up). */
function slotFor(spot: DropSpot, dragsFolder: boolean): HTMLElement | null {
  const key = spot.row ? `before:${spot.row}` : spot.before ? `before:${spot.before}` : `${dragsFolder ? "folders" : "connections"}:${spot.folder}`;
  return document.querySelector<HTMLElement>(`.drop-slot[data-slot="${CSS.escape(key)}"]`);
}

/** The row that lights up for a drop inside a folder ("" is the header). */
function folderEl(box: HTMLElement, id: string): HTMLElement | null {
  return box.querySelector<HTMLElement>(`[data-drop-folder="${CSS.escape(id)}"]`);
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

/** A folder drags like a connection, and can also go in front of or after another folder. */
export function beginFolderDrag(event: React.MouseEvent, options: DragOptions) {
  beginSidebarDrag(event, true, options);
}

function beginSidebarDrag(event: React.MouseEvent, dragsFolder: boolean, { allowed, onDrop, onSpring }: DragOptions) {
  if (event.button !== 0) return;
  const box = scroller();
  if (!box) return;
  const startX = event.clientX;
  const startY = event.clientY;
  const source = event.currentTarget as HTMLElement;
  let ghost: HTMLElement | null = null;
  let dragging = false;
  let snapshot: Snapshot | null = null;
  let lit: HTMLElement | null = null;
  let slot: HTMLElement | null = null;
  let targetKey = "";
  let spring = 0;
  let frame = 0;
  let pointer = { x: startX, y: startY };

  /** What is under the pointer now, read on the rows at rest. */
  const targetAt = (y: number): Target | null => {
    if (!snapshot) return null;
    const open = slot?.classList.contains("open") ? slot : null;
    let gap: { top: number; height: number } | null = null;
    if (open) {
      const r = open.getBoundingClientRect();
      const style = getComputedStyle(open);
      const before = parseFloat(style.marginTop) || 0;
      gap = { top: r.top - before, height: r.height + before + (parseFloat(style.marginBottom) || 0) };
    }
    const inList = restY(y, gap) - box.getBoundingClientRect().top + box.scrollTop;
    return resolveTarget(snapshot, inList, dragsFolder, allowed);
  };

  const show = (target: Target | null, force = false) => {
    const key = target ? JSON.stringify(target) : "";
    if (key === targetKey && !force) return;
    if (key !== targetKey) {
      window.clearTimeout(spring);
      const folder = target?.into ? folderEl(box, target.into) : null;
      if (target?.into && folder?.dataset.open === "false") {
        spring = window.setTimeout(() => {
          onSpring(target.into as string);
          // Its rows exist once it has drawn open: measure again, and show the spot anew.
          window.setTimeout(() => {
            snapshot = measure(box);
            show(targetAt(pointer.y), true);
          }, 80);
        }, SPRING_MS);
      }
    }
    targetKey = key;
    lit?.classList.remove(DROP_CLASS);
    lit = target?.into != null ? folderEl(box, target.into) : null;
    lit?.classList.add(DROP_CLASS);
    const next = target && target.into === null ? slotFor(target.spot, dragsFolder) : null;
    if (next !== slot) {
      slot?.classList.remove("open");
      slot = next;
      slot?.classList.add("open");
    }
  };

  const update = () => show(targetAt(pointer.y));

  /** Near the top or bottom of the list, it scrolls under the drag. */
  const autoScroll = () => {
    frame = 0;
    if (!dragging) return;
    const r = box.getBoundingClientRect();
    let step = 0;
    if (pointer.y < r.top + SCROLL_ZONE) step = -SCROLL_MAX * Math.min(1, (r.top + SCROLL_ZONE - pointer.y) / SCROLL_ZONE);
    else if (pointer.y > r.bottom - SCROLL_ZONE) step = SCROLL_MAX * Math.min(1, (pointer.y - (r.bottom - SCROLL_ZONE)) / SCROLL_ZONE);
    if (step !== 0) {
      const before = box.scrollTop;
      box.scrollTop += step;
      if (box.scrollTop !== before) update();
    }
    frame = requestAnimationFrame(autoScroll);
  };

  const move = (e: MouseEvent) => {
    if (!dragging && Math.hypot(e.clientX - startX, e.clientY - startY) < THRESHOLD) return;
    if (!dragging) {
      dragging = true;
      document.body.classList.add("dragging-connection");
      source.classList.add(SOURCE_CLASS);
      ghost = makeGhost(source);
      snapshot = measure(box);
      frame = requestAnimationFrame(autoScroll);
    }
    pointer = { x: e.clientX, y: e.clientY };
    if (ghost) ghost.style.transform = `translate3d(${pointer.x + 12}px, ${pointer.y + 10}px, 0)`;
    update();
  };

  /** The row is in its place: the slot goes without animating, or the list would jump twice. */
  const closeSlot = (open: HTMLElement | null) => {
    if (!open) return;
    open.classList.add("instant");
    open.classList.remove("open");
    window.setTimeout(() => open.classList.remove("instant"), 50);
  };

  /** The dragged card flies to where the row lands (the open gap, or the folder), and fades. */
  const settle = (card: HTMLElement | null, to: HTMLElement | null) => {
    if (!card) return;
    const r = to?.getBoundingClientRect();
    card.style.transition = `transform ${SETTLE_MS}ms cubic-bezier(0.2, 0.8, 0.2, 1), opacity ${SETTLE_MS}ms ease-out`;
    if (r) card.style.transform = `translate3d(${r.left + 8}px, ${r.top}px, 0)`;
    card.style.opacity = "0";
    window.setTimeout(() => card.remove(), SETTLE_MS + 30);
  };

  const stop = () => {
    window.removeEventListener("mousemove", move);
    window.removeEventListener("mouseup", finish);
    window.removeEventListener("keydown", key, true);
    window.clearTimeout(spring);
    if (frame) cancelAnimationFrame(frame);
    frame = 0;
    lit?.classList.remove(DROP_CLASS);
    document.body.classList.remove("dragging-connection");
    source.classList.remove(SOURCE_CLASS);
  };

  const key = (e: KeyboardEvent) => {
    if (e.key !== "Escape") return;
    e.preventDefault();
    e.stopPropagation();
    stop();
    slot?.classList.remove("open");
    settle(ghost, source);
    if (dragging) window.addEventListener("click", (c) => c.stopPropagation(), { capture: true, once: true });
  };

  function finish(e: MouseEvent) {
    pointer = { x: e.clientX, y: e.clientY };
    const target = dragging ? targetAt(pointer.y) : null;
    const open = slot;
    const into = lit;
    stop();
    if (!dragging) return;
    // The click that ends a drag must not connect (or fold the folder).
    window.addEventListener("click", (c) => c.stopPropagation(), { capture: true, once: true });
    if (!target) {
      open?.classList.remove("open");
      settle(ghost, source);
      return;
    }
    settle(ghost, open ?? into);
    void Promise.resolve(onDrop(target.spot)).finally(() => window.setTimeout(() => closeSlot(open)));
  }
  window.addEventListener("mousemove", move);
  window.addEventListener("mouseup", finish);
  window.addEventListener("keydown", key, true);
}
