// A modal sheet over the window, and a small context menu. Escape closes both.

import { Fragment, useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { Icon, type IconName } from "./Icon";

export function Sheet({
  children,
  onClose,
  width = 560,
  label,
}: {
  children: ReactNode;
  onClose: () => void;
  width?: number;
  label: string;
}) {
  const panel = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const key = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.stopPropagation();
        onClose();
      }
    };
    window.addEventListener("keydown", key, true);
    return () => window.removeEventListener("keydown", key, true);
  }, [onClose]);
  return (
    <div className="sheet-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className="sheet" role="dialog" aria-modal="true" aria-label={label} style={{ width }} ref={panel}>
        {children}
      </div>
    </div>
  );
}

export interface MenuItem {
  title: string;
  icon?: IconName;
  destructive?: boolean;
  disabled?: boolean;
  /** A check mark in the icon column: the current choice. */
  checked?: boolean;
  /** Shown on the right, like a shortcut. */
  detail?: string;
  /** The tooltip. */
  hint?: string;
  action?: () => void;
  /** A submenu: the item opens it (hover, click or Right Arrow) instead of acting. */
  items?: MenuItem[];
  /** A separator line instead of an item. */
  separator?: boolean;
}

export interface MenuState {
  x: number;
  y: number;
  items: MenuItem[];
}

const EDGE = 4;

const canFocus = (item: MenuItem) => !item.separator && !item.disabled;
const itemsAt = (root: MenuItem[], path: number[]) => path.reduce((items, i) => items[i]?.items ?? items, root);

function step(items: MenuItem[], from: number, direction: 1 | -1): number {
  if (!items.some(canFocus)) return -1;
  let i = from;
  do i = (i + direction + items.length) % items.length;
  while (!canFocus(items[i]));
  return i;
}

export function ContextMenu({ menu, onClose }: { menu: MenuState; onClose: () => void }) {
  const ref = useRef<HTMLDivElement | null>(null);
  // The open submenus (the item index in each panel) and the highlighted item of the deepest panel.
  const [path, setPath] = useState<number[]>([]);
  const [active, setActive] = useState(-1);
  const state = useRef({ path, active });
  state.current = { path, active };

  const activate = (item: MenuItem, depth: number, index: number) => {
    if (item.items) {
      setPath([...state.current.path.slice(0, depth), index]);
      setActive(-1);
      return;
    }
    onClose();
    item.action?.();
  };

  useEffect(() => {
    const key = (event: KeyboardEvent) => {
      const { path, active } = state.current;
      const items = itemsAt(menu.items, path);
      const handled = () => {
        event.preventDefault();
        event.stopPropagation();
      };
      switch (event.key) {
        case "Escape":
          handled();
          if (path.length === 0) return onClose();
          setActive(path[path.length - 1]);
          return setPath(path.slice(0, -1));
        case "ArrowDown":
        case "ArrowUp":
          handled();
          return setActive(step(items, active < 0 ? (event.key === "ArrowDown" ? -1 : 0) : active, event.key === "ArrowDown" ? 1 : -1));
        case "ArrowRight": {
          const item = items[active];
          if (!item?.items || item.disabled) return;
          handled();
          setPath([...path, active]);
          return setActive(step(item.items, -1, 1));
        }
        case "ArrowLeft":
          if (path.length === 0) return;
          handled();
          setActive(path[path.length - 1]);
          return setPath(path.slice(0, -1));
        case "Enter":
        case " ": {
          const item = items[active];
          if (!item || !canFocus(item)) return;
          handled();
          if (item.items) {
            setPath([...path, active]);
            return setActive(step(item.items, -1, 1));
          }
          onClose();
          return item.action?.();
        }
      }
    };
    const close = (event: Event) => {
      if (event.type === "mousedown" && ref.current?.contains(event.target as Node)) return;
      onClose();
    };
    window.addEventListener("mousedown", close, true);
    window.addEventListener("keydown", key, true);
    window.addEventListener("blur", close);
    window.addEventListener("resize", close);
    return () => {
      window.removeEventListener("mousedown", close, true);
      window.removeEventListener("keydown", key, true);
      window.removeEventListener("blur", close);
      window.removeEventListener("resize", close);
    };
  }, [menu, onClose]);

  return (
    <MenuPanel
      items={menu.items}
      depth={0}
      path={path}
      active={active}
      at={{ x: menu.x, y: menu.y }}
      panelRef={ref}
      onActivate={activate}
      onHover={(depth, index, item) => {
        const open = item.items && !item.disabled;
        setPath(open ? [...path.slice(0, depth), index] : path.slice(0, depth));
        setActive(open || item.disabled ? -1 : index);
      }}
      onLeave={(depth, index) => depth === path.length && active === index && setActive(-1)}
    />
  );
}

interface PanelProps {
  items: MenuItem[];
  depth: number;
  path: number[];
  active: number;
  /** The corner for the root menu, or the parent item for a submenu. */
  at: { x: number; y: number } | { anchor: () => HTMLElement | null };
  panelRef?: React.MutableRefObject<HTMLDivElement | null>;
  onActivate: (item: MenuItem, depth: number, index: number) => void;
  onHover: (depth: number, index: number, item: MenuItem) => void;
  onLeave: (depth: number, index: number) => void;
}

function MenuPanel(props: PanelProps) {
  const { items, depth, path, active, at, onActivate, onHover, onLeave } = props;
  const own = useRef<HTMLDivElement | null>(null);
  const rows = useRef<Array<HTMLElement | null>>([]);
  const [position, setPosition] = useState<{ left: number; top: number; visibility?: "hidden" }>({
    left: "x" in at ? at.x : 0,
    top: "y" in at ? at.y : 0,
    visibility: "x" in at ? undefined : "hidden",
  });
  useLayoutEffect(() => {
    const box = own.current?.getBoundingClientRect();
    if (!box) return;
    const clamp = (v: number, size: number, limit: number) => Math.max(EDGE, Math.min(v, limit - size - EDGE));
    if ("x" in at) {
      setPosition({ left: clamp(at.x, box.width, window.innerWidth), top: clamp(at.y, box.height, window.innerHeight) });
      return;
    }
    const row = at.anchor()?.getBoundingClientRect();
    if (!row) return;
    // To the right of the parent item, flipped to its left when there is no room.
    const right = row.right - EDGE;
    const left = right + box.width + EDGE > window.innerWidth ? row.left - box.width + EDGE : right;
    setPosition({ left: Math.max(EDGE, left), top: clamp(row.top - EDGE, box.height, window.innerHeight) });
  }, [items, at]);

  const deepest = depth === path.length;
  return (
    <div className="menu" role="menu" ref={(el) => { own.current = el; if (props.panelRef) props.panelRef.current = el; }} style={position} onContextMenu={(e) => e.preventDefault()}>
      {items.map((item, i) => {
        if (item.separator) return <div key={i} className="menu-separator" />;
        const opens = !!item.items;
        const submenu = opens && path[depth] === i && !item.disabled;
        const highlighted = submenu || (deepest && active === i);
        return (
          <Fragment key={i}>
            <button
              ref={(el) => { rows.current[i] = el; }}
              role="menuitem"
              aria-haspopup={opens || undefined}
              aria-expanded={opens ? submenu : undefined}
              className={`menu-item${item.destructive ? " destructive" : ""}${highlighted ? " active" : ""}`}
              disabled={item.disabled}
              title={item.hint}
              onMouseEnter={() => onHover(depth, i, item)}
              onMouseLeave={() => onLeave(depth, i)}
              onClick={() => onActivate(item, depth, i)}
            >
              <span className="menu-icon">{item.checked ? <Icon name="check" size={14} /> : item.icon && <Icon name={item.icon} size={14} />}</span>
              <span className="menu-title">{item.title}</span>
              {item.detail && <span className="menu-detail">{item.detail}</span>}
              {opens && (
                <span className="menu-chevron">
                  <Icon name="chevronRight" size={12} />
                </span>
              )}
            </button>
            {submenu && item.items && (
              <MenuPanel {...props} items={item.items} depth={depth + 1} panelRef={undefined} at={{ anchor: () => rows.current[i] }} />
            )}
          </Fragment>
        );
      })}
    </div>
  );
}
