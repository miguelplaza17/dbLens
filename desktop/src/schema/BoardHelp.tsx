// The keyboard button of the board's toolbar and its popover: the board's mouse
// and key gestures (BoardHelp in SchemaBoard.swift), plus the ones only this
// app has (Space + drag, the minimap, Alt to move freely on every system).

import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { Icon } from "../components/Icon";
import { t } from "../i18n";
import { isMac, keyLabel } from "../shortcuts";

function helpRows(): Array<[string, string]> {
  const mod = isMac ? "⌘" : "Ctrl+";
  return [
    [t("Drag a card"), t("Move it (and the rest of the selection)")],
    [t("Drag the background"), t("Move around the board")],
    [t("Space + drag"), t("Move around the board")],
    [t("Click or drag the minimap"), t("Move around the board")],
    [t("Shift + drag the background"), t("Select with a rectangle")],
    [t("Scroll, or pinch"), t("Zoom at the pointer")],
    [t("Shift-click"), t("Add to the selection")],
    [t("Double-click a card"), t("Open its rows")],
    [t("Double-click the board, or N"), t("Add a note")],
    [t("Arrow keys"), t("Move the board, or nudge the selection (Shift: further)")],
    [`${mod}A · Esc`, t("Select all · Clear the selection")],
    [isMac ? "Return" : "Enter", t("Open the selected table")],
    [t("Delete"), t("Remove the selected notes")],
    [`${keyLabel("zoomIn")} · ${keyLabel("zoomOut")} · ${keyLabel("zoomToFit")}`, t("Zoom in · Zoom out · Fit")],
    [isMac ? t("Option while dragging") : t("Alt while dragging"), t("Don't snap to the grid")],
  ];
}

export function BoardHelp() {
  const [open, setOpen] = useState(false);
  const button = useRef<HTMLButtonElement>(null);
  return (
    <>
      <button
        ref={button}
        className={`button icon-only${open ? " active" : ""}`}
        onClick={() => setOpen((v) => !v)}
        aria-haspopup="dialog"
        aria-expanded={open}
        title={t("Mouse and keyboard shortcuts for the board")}
        aria-label={t("Shortcuts")}
      >
        <Icon name="keyboard" />
      </button>
      {open && <Popover anchor={button} onClose={() => setOpen(false)} />}
    </>
  );
}

function Popover({ anchor, onClose }: { anchor: React.RefObject<HTMLButtonElement | null>; onClose: () => void }) {
  const root = useRef<HTMLDivElement>(null);
  const [place, setPlace] = useState<{ right: number; top: number } | null>(null);

  // Under the button, right edges together.
  useLayoutEffect(() => {
    const rect = anchor.current?.getBoundingClientRect();
    if (rect) setPlace({ right: Math.max(8, window.innerWidth - rect.right), top: rect.bottom + 4 });
  }, [anchor]);

  useEffect(() => {
    const away = (event: MouseEvent) => {
      const target = event.target as Node;
      if (!root.current?.contains(target) && !anchor.current?.contains(target)) onClose();
    };
    const key = (event: KeyboardEvent) => event.key === "Escape" && onClose();
    window.addEventListener("mousedown", away);
    window.addEventListener("keydown", key);
    return () => {
      window.removeEventListener("mousedown", away);
      window.removeEventListener("keydown", key);
    };
  }, [anchor, onClose]);

  return (
    <div ref={root} className="board-help" role="dialog" aria-label={t("Shortcuts")} style={place ?? { visibility: "hidden" }}>
      <dl>
        {helpRows().map(([keys, what]) => (
          <div key={keys + what} className="board-help-row">
            <dt>{keys}</dt>
            <dd>{what}</dd>
          </div>
        ))}
      </dl>
    </div>
  );
}
