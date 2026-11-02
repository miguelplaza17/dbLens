// Line icons drawn in currentColor on a 16-point box, so they take the text
// color of whatever holds them (no colors here).

import type { CSSProperties } from "react";

const paths: Record<string, string> = {
  database: "M8 2.5c3 0 5 .9 5 2s-2 2-5 2-5-.9-5-2 2-2 5-2ZM3 4.5v7c0 1.1 2 2 5 2s5-.9 5-2v-7M3 8c0 1.1 2 2 5 2s5-.9 5-2",
  table: "M2.5 3.5h11v9h-11zM2.5 6.5h11M2.5 9.5h11M6.5 6.5v6",
  view: "M1.5 8s2.4-4.5 6.5-4.5S14.5 8 14.5 8 12.1 12.5 8 12.5 1.5 8 1.5 8ZM8 10a2 2 0 1 0 0-4 2 2 0 0 0 0 4Z",
  materialized: "M2.5 3.5h11v9h-11zM2.5 6.5h11M8.5 7.5l-2 2.5h3l-2 2",
  foreign: "M2.5 3.5h11v9h-11zM2.5 6.5h11M7 9.5h4M9.5 8l1.5 1.5L9.5 11",
  key: "M10 2.5a3.5 3.5 0 1 1-2.9 5.5L2.5 12.6v1h2v-1.5h1.5V10.6l1.1-1.1A3.5 3.5 0 0 1 10 2.5ZM10.8 5.2h.01",
  folder: "M1.5 4.5a1 1 0 0 1 1-1h3.2l1.5 1.5h6.3a1 1 0 0 1 1 1v6.5a1 1 0 0 1-1 1h-11a1 1 0 0 1-1-1z",
  folderPlus: "M1.5 4.5a1 1 0 0 1 1-1h3.2l1.5 1.5h6.3a1 1 0 0 1 1 1v6.5a1 1 0 0 1-1 1h-11a1 1 0 0 1-1-1zM8 7.5v4M6 9.5h4",
  plus: "M8 3v10M3 8h10",
  minus: "M3 8h10",
  schema: "M1.5 2.5h5v4h-5zM9.5 9.5h5v4h-5zM6.5 4.5H8a1 1 0 0 1 1 1v5a1 1 0 0 0 1 1M1.5 4.5h5M9.5 11.5h5",
  chevronRight: "M6 3.5 10.5 8 6 12.5",
  chevronDown: "M3.5 6 8 10.5 12.5 6",
  chevronLeft: "M10 3.5 5.5 8 10 12.5",
  search: "M7 12a5 5 0 1 0 0-10 5 5 0 0 0 0 10ZM10.7 10.7l3.3 3.3",
  play: "M4.5 2.8v10.4L13 8z",
  stop: "M4 4h8v8H4z",
  refresh: "M13 3v3.5H9.5M3 13V9.5h3.5M12.6 6.5A5 5 0 0 0 3.6 5.2M3.4 9.5a5 5 0 0 0 9 1.3",
  panelBottom: "M2.5 2.5h11v11h-11zM2.5 9.5h11",
  sidebar: "M2.5 2.5h11v11h-11zM6 2.5v11",
  copy: "M5.5 5.5h8v8h-8zM10.5 5.5v-3h-8v8h3",
  trash: "M2.5 4h11M6 4V2.5h4V4M4 4l.7 9.5h6.6L12 4M6.8 6.5v5M9.2 6.5v5",
  close: "M4 4l8 8M12 4l-8 8",
  check: "M3 8.5l3 3 7-7",
  checkCircle: "M8 14.5a6.5 6.5 0 1 0 0-13 6.5 6.5 0 0 0 0 13ZM5 8.2l2 2 4-4.2",
  xCircle: "M8 14.5a6.5 6.5 0 1 0 0-13 6.5 6.5 0 0 0 0 13ZM5.8 5.8l4.4 4.4M10.2 5.8l-4.4 4.4",
  alert: "M8 2 1.5 13.5h13zM8 6.5v3.5M8 11.8h.01",
  sun: "M8 11a3 3 0 1 0 0-6 3 3 0 0 0 0 6ZM8 1v1.5M8 13.5V15M1 8h1.5M13.5 8H15M3 3l1 1M12 12l1 1M3 13l1-1M12 4l1-1",
  moon: "M13.5 9.5A5.5 5.5 0 0 1 6.5 2.5a5.5 5.5 0 1 0 7 7Z",
  terminal: "M2.5 2.5h11v11h-11zM5 6l2 2-2 2M8.5 10.5H11",
  rehearse: "M6 2v4.5L2.8 12.4a.8.8 0 0 0 .7 1.1h9a.8.8 0 0 0 .7-1.1L10 6.5V2M5 2h6M4.5 10h7",
  atomic: "M8 1.8 13.5 4.8v6.4L8 14.2 2.5 11.2V4.8zM2.5 4.8 8 8l5.5-3.2M8 8v6.2",
  edit: "M10.5 2.5l3 3-8 8h-3v-3zM9 4l3 3",
  connect: "M9.5 1.5 4 9h4l-1.5 5.5L12 7H8z",
  disconnect: "M2 2l12 12M9.5 1.5 8 4M12 7l-1.6 2.2M6.5 14.5 8 9H4l1.3-1.8",
  more: "M3.5 8h.01M8 8h.01M12.5 8h.01",
  export: "M8 10V2M5 5l3-3 3 3M3 9v4.5h10V9",
  sortUp: "M8 4l3.5 5h-7z",
  sortDown: "M8 12l3.5-5h-7z",
  history: "M2.5 8a5.5 5.5 0 1 0 1.7-4M2.5 2.5v3h3M8 5v3.2l2 1.3",
  back: "M13 8H3M7 4 3 8l4 4",
  forward: "M3 8h10M9 4l4 4-4 4",
  keyboard: "M1.5 4h13v8h-13zM4 6.5h.01M6.5 6.5h.01M9 6.5h.01M11.5 6.5h.01M5 9.5h6",
};

export type IconName = keyof typeof paths;

export function Icon({ name, size = 16, style }: { name: IconName; size?: number; style?: CSSProperties }) {
  const filled = name === "play" || name === "stop" || name === "sortUp" || name === "sortDown";
  return (
    <svg
      className="icon"
      width={size}
      height={size}
      viewBox="0 0 16 16"
      fill={filled ? "currentColor" : "none"}
      stroke="currentColor"
      strokeWidth={filled ? 0 : 1.3}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      style={style}
    >
      <path d={paths[name]} />
    </svg>
  );
}
