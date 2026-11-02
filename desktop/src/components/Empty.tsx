// An empty state: an icon, a title, and a line or two about what to do.

import type { ReactNode } from "react";
import { Icon, type IconName } from "./Icon";

export function Empty({ icon, title, children }: { icon: IconName; title: string; children?: ReactNode }) {
  return (
    <div className="empty">
      <Icon name={icon} size={40} />
      <h2>{title}</h2>
      {children && <div className="empty-detail">{children}</div>}
    </div>
  );
}
