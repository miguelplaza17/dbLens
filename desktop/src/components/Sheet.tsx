// A modal sheet over the window, and a small context menu. Escape closes both.

import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
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
  /** Shown on the right, like a shortcut. */
  detail?: string;
  action?: () => void;
  /** A separator line instead of an item. */
  separator?: boolean;
}

export interface MenuState {
  x: number;
  y: number;
  items: MenuItem[];
}

export function ContextMenu({ menu, onClose }: { menu: MenuState; onClose: () => void }) {
  const ref = useRef<HTMLDivElement>(null);
  const [position, setPosition] = useState({ left: menu.x, top: menu.y });
  useLayoutEffect(() => {
    const box = ref.current?.getBoundingClientRect();
    if (!box) return;
    setPosition({
      left: Math.max(4, Math.min(menu.x, window.innerWidth - box.width - 4)),
      top: Math.max(4, Math.min(menu.y, window.innerHeight - box.height - 4)),
    });
  }, [menu]);
  useEffect(() => {
    const close = (event: Event) => {
      if (event instanceof KeyboardEvent && event.key !== "Escape") return;
      if (event.type === "mousedown" && ref.current?.contains(event.target as Node)) return;
      onClose();
    };
    window.addEventListener("mousedown", close, true);
    window.addEventListener("keydown", close, true);
    window.addEventListener("blur", close);
    window.addEventListener("resize", close);
    return () => {
      window.removeEventListener("mousedown", close, true);
      window.removeEventListener("keydown", close, true);
      window.removeEventListener("blur", close);
      window.removeEventListener("resize", close);
    };
  }, [onClose]);
  return (
    <div className="menu" role="menu" ref={ref} style={position} onContextMenu={(e) => e.preventDefault()}>
      {menu.items.map((item, i) =>
        item.separator ? (
          <div key={i} className="menu-separator" />
        ) : (
          <button
            key={i}
            role="menuitem"
            className={`menu-item${item.destructive ? " destructive" : ""}`}
            disabled={item.disabled}
            onClick={() => {
              onClose();
              item.action?.();
            }}
          >
            <span className="menu-icon">{item.icon && <Icon name={item.icon} size={14} />}</span>
            <span className="menu-title">{item.title}</span>
            {item.detail && <span className="menu-detail">{item.detail}</span>}
          </button>
        ),
      )}
    </div>
  );
}
