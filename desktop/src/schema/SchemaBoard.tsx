// The schema board (SchemaBoard.swift): every table as a card with its
// columns and key markers, foreign keys as curves between them, on an endless
// canvas. Drag a card to move it, drag the background to move the board,
// scroll or pinch to zoom at the pointer, double-click a card (or Return) to
// open its rows. Shift-drag the background selects with a rectangle, Space +
// drag moves the board, N or a double-click on the background adds a sticky
// note. Positions, colors, notes, folded cards and the view are kept per
// connection.

import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import { t } from "../i18n";
import { useModel, type AppModel } from "../model";
import { withKeys } from "../shortcuts";
import type { MenuItem } from "../components/Sheet";
import { Empty } from "../components/Empty";
import { Icon } from "../components/Icon";
import { BoardHelp } from "./BoardHelp";
import { BOARD_COLORS, intersects, isNoteKey, NOTE_HEIGHT, NOTE_WIDTH, noteKey, snap, tableId, type Point, type Rect } from "./layout";
import {
  drawBoard,
  drawMinimap,
  hitItem,
  noteRect,
  cardRect,
  MINIMAP,
  minimapMapping,
  readColors,
  unmap,
  visibleRect,
  type Colors,
  type Mapping,
  type Marks,
} from "./draw";
import { schema as state, useSchema, ZOOM_STEP, type Board, type BoardView, type Note } from "./store";
import "./schema.css";

export function SchemaBoard() {
  const model = useModel();
  const s = useSchema();
  const current = s.sessionId === model.session?.sessionId;

  useEffect(() => {
    if (!current || (!s.graph && !s.loading && !s.error)) void s.load(model);
  }, [current, s, model]);

  if (!current || (!s.graph && !s.error)) {
    return (
      <div className="center-fill">
        <span className="spinner" />
      </div>
    );
  }
  if (s.error) {
    return (
      <Empty icon="alert" title={t("Couldn't read the schema")}>
        <span className="selectable pre">{s.error}</span>
      </Empty>
    );
  }
  if (!s.board || s.view.tables.length === 0) {
    return (
      <Empty icon="schema" title={t("No tables")}>
        <p>{t("This database has no tables to draw yet.")}</p>
      </Empty>
    );
  }
  return (
    <div className="schema-board">
      <Controls board={s.board} view={s.view} />
      <BoardCanvas key={s.board.key} model={model} board={s.board} view={s.view} showSchema={!s.schema && s.view.schemas.length > 1} theme={model.theme} />
    </div>
  );
}

function plural(count: number, one: string, many: string) {
  return count === 1 ? t(one) : t(many, count);
}

function Controls({ board, view }: { board: Board; view: BoardView }) {
  const s = useSchema();
  // The zoom label follows the board, which changes without React.
  const zoom = useSyncExternalStore(
    useCallback((listener: () => void) => board.onChange(listener), [board]),
    () => board.zoom,
  );
  const folded = s.allCollapsed;
  return (
    <div className="schema-controls">
      {view.schemas.length > 1 && (
        <select className="button" value={s.schema ?? ""} onChange={(e) => s.setSchema(e.target.value || null)} aria-label={t("Schema")}>
          <option value="">{t("All Schemas")}</option>
          {view.schemas.map((name) => (
            <option key={name} value={name}>
              {name}
            </option>
          ))}
        </select>
      )}
      <span className="schema-count">
        {plural(view.tables.length, "1 table", "%lld tables")} · {plural(view.links.length, "1 relation", "%lld relations")}
      </span>
      {s.loading && <span className="spinner small" />}
      <span className="spacer" />
      <button className="button" onClick={() => board.addNoteHere?.()} title={t("Add a sticky note (N, or double-click the board)")}>
        {t("Add Note")}
      </button>
      <button className="button" onClick={() => s.toggleAllCollapsed()}>
        {folded ? t("Show All Columns") : t("Key Columns Only")}
      </button>
      <button className="button" onClick={() => s.arrange()} title={t("Lay the cards out again automatically")}>
        {t("Arrange")}
      </button>
      <div className="control-group">
        <button className="button icon-only" onClick={() => board.zoomBy(1 / ZOOM_STEP)} title={withKeys(t("Zoom Out"), "zoomOut")} aria-label={t("Zoom Out")}>
          <Icon name="minus" />
        </button>
        <button className="button" onClick={() => s.fit()} title={withKeys(t("Zoom to Fit"), "zoomToFit")}>
          {t("Fit")}
        </button>
        <button className="button icon-only" onClick={() => board.zoomBy(ZOOM_STEP)} title={withKeys(t("Zoom In"), "zoomIn")} aria-label={t("Zoom In")}>
          <Icon name="plus" />
        </button>
      </div>
      <span className="schema-zoom">{Math.round(zoom * 100)}%</span>
      <BoardHelp />
    </div>
  );
}

interface Drag {
  kind: "cards" | "pan" | "marquee";
  pointer: number;
  start: Point;
  moved: boolean;
  /** Board positions (cards) or the pan (pan) when the drag started. */
  origins: Map<string, Point>;
  pan: Point;
  /** The card pressed, for a click without a move. */
  card: string | null;
  additive: boolean;
  /** The card was selected before this press (Shift-click on it takes it out). */
  wasIn: boolean;
  /** The selection a marquee adds to. */
  base: Set<string>;
}

function BoardCanvas({ model, board, view, showSchema, theme }: { model: AppModel; board: Board; view: BoardView; showSchema: boolean; theme: string }) {
  const wrap = useRef<HTMLDivElement>(null);
  const canvas = useRef<HTMLCanvasElement>(null);
  const mini = useRef<HTMLCanvasElement>(null);
  const marks = useRef<Marks>({ hovered: null, selection: new Set(), order: [], marquee: null, editing: null });
  const space = useRef(false);
  const [editing, setEditing] = useState<string | null>(null);
  marks.current.editing = editing;
  const drag = useRef<Drag | null>(null);
  const miniDrag = useRef<Mapping | null>(null);
  const colors = useRef<Colors | null>(null);
  const frame = useRef(0);

  // Drawing order: the view's tables, with the ones touched last on top.
  useMemo(() => {
    const ids = new Set(view.tables.map(tableId));
    const kept = marks.current.order.filter((id) => ids.has(id));
    const keptSet = new Set(kept);
    marks.current.order = [...view.tables.map(tableId).filter((id) => !keptSet.has(id)), ...kept];
    for (const id of [...marks.current.selection]) if (!ids.has(id) && !(isNoteKey(id) && board.note(id))) marks.current.selection.delete(id);
  }, [view, board]);

  const paint = () => {
    frame.current = 0;
    const el = canvas.current;
    const ctx = el?.getContext("2d");
    if (!el || !ctx || board.viewport.width === 0) return;
    colors.current ??= readColors();
    const dpr = window.devicePixelRatio || 1;
    drawBoard(ctx, dpr, board, view, marks.current, colors.current, showSchema);
    const m = mini.current?.getContext("2d");
    if (m) drawMinimap(m, dpr, board, view, marks.current, colors.current, miniDrag.current ?? mapping());
  };
  // The latest paint (it reads this render's view), for frames and listeners.
  const latest = useRef(paint);
  latest.current = paint;
  const request = () => {
    if (!frame.current) frame.current = requestAnimationFrame(() => latest.current());
  };
  const mapping = () => minimapMapping(state.bounds(), visibleRect(board));

  useEffect(() => board.onChange(request), [board]);
  // A new view or schema label: draw again.
  useEffect(request, [view, showSchema, editing]);
  useEffect(() => () => cancelAnimationFrame(frame.current), []);

  // A new theme or accent (Settings sets a class on <html>): read the colors again.
  useLayoutEffect(() => {
    colors.current = null;
    request();
    const observer = new MutationObserver(() => {
      colors.current = null;
      request();
    });
    observer.observe(document.documentElement, { attributes: true, attributeFilter: ["class", "data-theme"] });
    return () => observer.disconnect();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [theme]);

  // The canvas follows the board's size; the first time, everything is fitted in.
  useLayoutEffect(() => {
    const el = wrap.current;
    if (!el) return;
    const resize = () => {
      const { width, height } = el.getBoundingClientRect();
      const dpr = window.devicePixelRatio || 1;
      board.viewport = { width, height };
      for (const [c, w, h] of [
        [canvas.current, width, height],
        [mini.current, MINIMAP.width, MINIMAP.height],
      ] as const) {
        if (!c) continue;
        c.width = Math.round(w * dpr);
        c.height = Math.round(h * dpr);
        c.style.width = `${w}px`;
        c.style.height = `${h}px`;
      }
      if (board.needsFit) state.fit();
      latest.current();
    };
    const observer = new ResizeObserver(resize);
    observer.observe(el);
    resize();
    canvas.current?.focus({ preventScroll: true });
    return () => observer.disconnect();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [board]);

  // Wheel zooms at the pointer (not passive, so the page doesn't scroll).
  useEffect(() => {
    const el = canvas.current;
    if (!el) return;
    const wheel = (e: WheelEvent) => {
      e.preventDefault();
      const unit = e.deltaMode === 1 ? 16 : e.deltaMode === 2 ? board.viewport.height : 1;
      const dy = e.deltaY * unit;
      const factor = Math.exp(-dy * (e.ctrlKey ? 0.01 : 0.0015));
      board.zoomBy(factor, local(e));
    };
    el.addEventListener("wheel", wheel, { passive: false });
    return () => el.removeEventListener("wheel", wheel);
  }, [board]);

  const local = (e: { clientX: number; clientY: number }): Point => {
    const r = canvas.current!.getBoundingClientRect();
    return { x: e.clientX - r.left, y: e.clientY - r.top };
  };
  const hit = (e: { clientX: number; clientY: number }) => hitItem(board, view, marks.current, board.toWorld(local(e)));

  const toFront = (ids: Iterable<string>) => {
    const lift = new Set(ids);
    marks.current.order = [...marks.current.order.filter((id) => !lift.has(id)), ...marks.current.order.filter((id) => lift.has(id))];
  };

  /** A new note with its corner at a board point, selected and open for writing. */
  const addNote = (at: Point) => {
    const note = board.addNote(at);
    marks.current.selection = new Set([noteKey(note)]);
    setEditing(noteKey(note));
  };
  useEffect(() => {
    board.addNoteHere = () => addNote(board.toWorld({ x: board.viewport.width / 2, y: board.viewport.height / 2 }));
    return () => {
      board.addNoteHere = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [board]);

  const onPointerDown = (e: React.PointerEvent<HTMLCanvasElement>) => {
    if (e.button !== 0) return;
    e.currentTarget.focus({ preventScroll: true });
    e.currentTarget.setPointerCapture(e.pointerId);
    const id = space.current ? null : hit(e);
    const m = marks.current;
    const additive = e.shiftKey || e.ctrlKey || e.metaKey;
    const base = { pointer: e.pointerId, start: local(e), moved: false, origins: new Map<string, Point>(), pan: { ...board.pan }, card: null, additive, wasIn: false, base: new Set<string>() };
    if (id) {
      const wasIn = m.selection.has(id);
      if (additive) {
        // Shift-click toggles on pointer up, so a shift-drag still moves the group.
        m.selection.add(id);
      } else if (!wasIn) {
        m.selection = new Set([id]);
      }
      const origins = new Map<string, Point>();
      for (const sid of m.selection) {
        const p = board.pointOf(sid);
        if (p) origins.set(sid, { ...p });
      }
      toFront(m.selection);
      drag.current = { ...base, kind: "cards", origins, card: id, wasIn };
    } else if (e.shiftKey && !e.ctrlKey && !e.metaKey && !space.current) {
      // Shift-drag on the background: select with a rectangle, adding to what is selected.
      drag.current = { ...base, kind: "marquee", base: new Set(m.selection) };
    } else {
      drag.current = { ...base, kind: "pan" };
      e.currentTarget.style.cursor = "grabbing";
    }
    request();
  };

  const onPointerMove = (e: React.PointerEvent<HTMLCanvasElement>) => {
    const d = drag.current;
    const p = local(e);
    if (!d || d.pointer !== e.pointerId) {
      const id = space.current ? null : hit(e);
      e.currentTarget.style.cursor = id ? "default" : "grab";
      const table = id && !isNoteKey(id) ? id : null;
      if (table !== marks.current.hovered) {
        marks.current.hovered = table;
        request();
      }
      return;
    }
    const dx = p.x - d.start.x;
    const dy = p.y - d.start.y;
    if (!d.moved && Math.hypot(dx, dy) < 3) return;
    d.moved = true;
    if (d.kind === "pan") {
      board.pan = { x: d.pan.x + dx, y: d.pan.y + dy };
      board.changed(false);
      return;
    }
    if (d.kind === "marquee") {
      const screen: Rect = { x: Math.min(d.start.x, p.x), y: Math.min(d.start.y, p.y), width: Math.abs(dx), height: Math.abs(dy) };
      const corner = board.toWorld({ x: screen.x, y: screen.y });
      const world: Rect = { x: corner.x, y: corner.y, width: screen.width / board.zoom, height: screen.height / board.zoom };
      const chosen = new Set(d.base);
      for (const table of view.tables) {
        const r = cardRect(board, view, table);
        if (r && intersects(r, world)) chosen.add(tableId(table));
      }
      for (const note of board.notes) if (intersects(noteRect(note), world)) chosen.add(noteKey(note));
      marks.current.marquee = screen;
      marks.current.selection = chosen;
      request();
      return;
    }
    // Snap to the 8-unit grid; Alt moves freely.
    for (const [id, origin] of d.origins) {
      const x = origin.x + dx / board.zoom;
      const y = origin.y + dy / board.zoom;
      board.moveTo(id, e.altKey ? { x, y } : { x: snap(x), y: snap(y) });
    }
    board.changed(false);
  };

  const onPointerUp = (e: React.PointerEvent<HTMLCanvasElement>) => {
    const d = drag.current;
    if (!d || d.pointer !== e.pointerId) return;
    drag.current = null;
    e.currentTarget.style.cursor = d.kind === "cards" ? "default" : "grab";
    const m = marks.current;
    m.marquee = null;
    if (d.moved) {
      if (d.kind !== "marquee") board.save();
    } else if (d.kind === "marquee") {
      // A Shift-click on empty space: nothing changes.
    } else if (d.kind === "pan") {
      if (!d.additive) m.selection = new Set();
    } else if (d.card && d.additive) {
      if (d.wasIn) m.selection.delete(d.card);
    } else if (d.card) {
      m.selection = new Set([d.card]);
    }
    request();
  };

  const onDoubleClick = (e: React.MouseEvent<HTMLCanvasElement>) => {
    const id = hit(e);
    if (!id) return addNote(board.toWorld(local(e)));
    if (isNoteKey(id)) return setEditing(id);
    const table = view.byId.get(id);
    if (table) void state.open(model, table);
  };

  const colorItems = (ids: Set<string>, id: string): MenuItem[] => {
    const current = isNoteKey(id) ? board.note(id)?.color : board.colors.get(id) ?? 0;
    return [
      { title: "", separator: true },
      {
        title: t("Color"),
        items: BOARD_COLORS.map((c, index) => ({ title: t(c.name), checked: index === current, action: () => board.setColor(ids, index) })),
      },
    ];
  };

  const onContextMenu = (e: React.MouseEvent<HTMLCanvasElement>) => {
    const id = hit(e);
    const m = marks.current;
    if (!id) {
      const at = board.toWorld(local(e));
      return model.showMenu(e, [{ title: t("Add Note"), action: () => addNote(at) }]);
    }
    if (!m.selection.has(id)) m.selection = new Set([id]);
    request();
    const ids = new Set(m.selection);
    if (isNoteKey(id)) {
      return model.showMenu(e, [
        { title: t("Edit Note"), action: () => setEditing(id) },
        ...colorItems(ids, id),
        { title: "", separator: true },
        {
          title: t("Delete Note"),
          destructive: true,
          action: () => {
            board.removeNotes([id]);
            m.selection.delete(id);
            request();
          },
        },
      ]);
    }
    const table = view.byId.get(id);
    if (!table) return;
    model.showMenu(e, [
      { title: t("Open Rows"), icon: "table", action: () => void state.open(model, table) },
      {
        title: board.collapsed.has(id) ? t("Show All Columns") : t("Key Columns Only"),
        action: () => state.toggleCollapsed(id),
      },
      ...colorItems(ids, id),
    ]);
  };

  const setSpace = (on: boolean, el: HTMLCanvasElement) => {
    if (space.current === on) return;
    space.current = on;
    if (!drag.current) el.style.cursor = on ? "grab" : "default";
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLCanvasElement>) => {
    const m = marks.current;
    if (e.key === " " && !e.ctrlKey && !e.metaKey && !e.altKey) {
      e.preventDefault();
      return setSpace(true, e.currentTarget);
    }
    const arrows: Record<string, [number, number]> = { ArrowLeft: [-1, 0], ArrowRight: [1, 0], ArrowUp: [0, -1], ArrowDown: [0, 1] };
    const dir = arrows[e.key];
    if (dir && !e.ctrlKey && !e.metaKey && !e.altKey) {
      e.preventDefault();
      if (m.selection.size === 0) {
        const step = e.shiftKey ? 240 : 48;
        board.pan = { x: board.pan.x - dir[0] * step, y: board.pan.y - dir[1] * step };
      } else {
        const step = e.shiftKey ? 40 : 8;
        for (const id of m.selection) {
          const p = board.pointOf(id);
          if (p) board.moveTo(id, { x: p.x + dir[0] * step, y: p.y + dir[1] * step });
        }
      }
      board.changed();
      return;
    }
    if (e.key === "Delete" || e.key === "Backspace") {
      // The board's own: Delete removes selected notes, never rows behind the board.
      e.preventDefault();
      e.stopPropagation();
      const notes = [...m.selection].filter(isNoteKey);
      if (notes.length === 0) return;
      board.removeNotes(notes);
      for (const id of notes) m.selection.delete(id);
      request();
    } else if (e.key === "Escape" && m.selection.size > 0) {
      e.preventDefault();
      m.selection = new Set();
      request();
    } else if (e.key === "Enter" && m.selection.size === 1) {
      const table = view.byId.get([...m.selection][0]);
      if (table) {
        e.preventDefault();
        void state.open(model, table);
      }
    } else if (e.key.toLowerCase() === "a" && (e.ctrlKey || e.metaKey) && !e.shiftKey && !e.altKey) {
      e.preventDefault();
      m.selection = new Set([...view.tables.map(tableId), ...board.notes.map(noteKey)]);
      request();
    } else if (e.key.toLowerCase() === "n" && !e.ctrlKey && !e.metaKey && !e.altKey && !e.shiftKey) {
      e.preventDefault();
      board.addNoteHere?.();
    }
  };

  const onMiniPointer = (e: React.PointerEvent<HTMLCanvasElement>) => {
    if (e.type === "pointerdown") {
      e.currentTarget.setPointerCapture(e.pointerId);
      miniDrag.current = mapping();
    } else if (!miniDrag.current) {
      return;
    }
    const r = e.currentTarget.getBoundingClientRect();
    board.center(unmap(miniDrag.current, { x: e.clientX - r.left, y: e.clientY - r.top }));
  };
  const endMini = () => {
    miniDrag.current = null;
    request();
  };

  return (
    <div className="schema-canvas" ref={wrap}>
      <canvas
        ref={canvas}
        tabIndex={0}
        aria-label={t("Schema")}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onPointerCancel={onPointerUp}
        onPointerLeave={() => {
          if (!drag.current && marks.current.hovered) {
            marks.current.hovered = null;
            request();
          }
        }}
        onDoubleClick={onDoubleClick}
        onContextMenu={onContextMenu}
        onKeyDown={onKeyDown}
        onKeyUp={(e) => e.key === " " && setSpace(false, e.currentTarget)}
        onBlur={(e) => setSpace(false, e.currentTarget)}
      />
      <canvas
        ref={mini}
        className="schema-minimap"
        title={t("Click or drag to move around the board")}
        onPointerDown={onMiniPointer}
        onPointerMove={onMiniPointer}
        onPointerUp={endMini}
        onPointerCancel={endMini}
      />
      {editing && board.note(editing) && (
        <NoteEditor
          key={editing}
          board={board}
          note={board.note(editing)!}
          onDone={() => {
            setEditing(null);
            canvas.current?.focus({ preventScroll: true });
          }}
        />
      )}
    </div>
  );
}

/** A note's text field, laid over the canvas at the note's place and size. */
function NoteEditor({ board, note, onDone }: { board: Board; note: Note; onDone: () => void }) {
  const [, redraw] = useState(0);
  const [draft, setDraft] = useState(note.text);
  const field = useRef<HTMLTextAreaElement>(null);
  const done = useRef(false);
  // The board moves under it (pan, zoom, a nudge) without React.
  useEffect(() => board.onChange(() => redraw((n) => n + 1)), [board]);
  useEffect(() => {
    const el = field.current;
    el?.focus({ preventScroll: true });
    el?.setSelectionRange(el.value.length, el.value.length);
  }, []);

  const commit = () => {
    if (done.current) return;
    done.current = true;
    note.text = draft;
    board.changed();
    onDone();
  };
  const z = board.zoom;
  const fill = BOARD_COLORS[note.color]?.hex;
  return (
    <textarea
      ref={field}
      className="schema-note-editor"
      value={draft}
      spellCheck={false}
      aria-label={t("Edit Note")}
      onChange={(e) => setDraft(e.target.value)}
      onBlur={commit}
      onKeyDown={(e) => {
        e.stopPropagation();
        if (e.key === "Escape") {
          e.preventDefault();
          commit();
        }
      }}
      style={{
        left: board.pan.x + note.x * z,
        top: board.pan.y + note.y * z,
        width: NOTE_WIDTH * z,
        height: NOTE_HEIGHT * z,
        padding: 12 * z,
        borderRadius: 8 * z,
        fontSize: 14 * z,
        lineHeight: `${18 * z}px`,
        background: fill ? `color-mix(in srgb, ${fill} 85%, transparent)` : "#3A3A40",
      }}
    />
  );
}
