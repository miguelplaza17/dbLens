// The canvas grid's colors and fonts, read from the CSS variables in theme.css
// (no color is written here). Canvas fills need opaque colors for row
// backgrounds, so translucent variables (stripes, pending edits) are blended
// over the surface color here.

import type { Theme } from "@glideapps/glide-data-grid";

type RGBA = [number, number, number, number];

let probe: CanvasRenderingContext2D | null = null;

/** Any CSS color as RGBA (0–255, alpha 0–1), through a 1×1 canvas. */
function parse(color: string): RGBA {
  const hex = color.trim().match(/^#([0-9a-f]{3,8})$/i);
  if (hex) {
    let h = hex[1];
    if (h.length <= 4) h = [...h].map((c) => c + c).join("");
    const n = (i: number) => parseInt(h.slice(i, i + 2), 16);
    return [n(0), n(2), n(4), h.length === 8 ? n(6) / 255 : 1];
  }
  const rgb = color.match(/rgba?\(\s*([\d.]+)[\s,]+([\d.]+)[\s,]+([\d.]+)(?:[\s,/]+([\d.]+%?))?\s*\)/i);
  if (rgb) {
    const a = rgb[4] === undefined ? 1 : rgb[4].endsWith("%") ? parseFloat(rgb[4]) / 100 : parseFloat(rgb[4]);
    return [Number(rgb[1]), Number(rgb[2]), Number(rgb[3]), a];
  }
  probe ??= document.createElement("canvas").getContext("2d", { willReadFrequently: true });
  if (!probe) return [0, 0, 0, 0];
  probe.clearRect(0, 0, 1, 1);
  probe.fillStyle = color;
  probe.fillRect(0, 0, 1, 1);
  const d = probe.getImageData(0, 0, 1, 1).data;
  return [d[0], d[1], d[2], d[3] / 255];
}

const css = ([r, g, b, a]: RGBA) => (a >= 1 ? `rgb(${r}, ${g}, ${b})` : `rgba(${r}, ${g}, ${b}, ${a.toFixed(3)})`);

/** `top` painted over an opaque `bottom`. */
function over(top: string, bottom: string): string {
  const t = parse(top);
  const b = parse(bottom);
  const mix = (i: number) => Math.round(t[i] * t[3] + b[i] * (1 - t[3]));
  return css([mix(0), mix(1), mix(2), 1]);
}

export interface GridColors {
  theme: Partial<Theme>;
  /** Odd rows. */
  stripe: string;
  /** A cell with an edit not yet saved. */
  pending: string;
  /** A row whose save failed. */
  failed: string;
  null: string;
  text: string;
  textSecondary: string;
  textTertiary: string;
  accentText: string;
  headerBg: string;
  separator: string;
  fontUI: string;
  fontMono: string;
  captionSize: string;
  monoSize: string;
}

/** Reads the variables now (call again after the theme changes). */
export function readGridColors(): GridColors {
  const style = getComputedStyle(document.documentElement);
  const v = (name: string) => style.getPropertyValue(name).trim() || "transparent";
  const surface = css(parse(v("--surface")));
  const window = over(v("--window"), surface);
  const text = v("--text");
  const fontUI = style.getPropertyValue("--font-ui").trim() || "sans-serif";
  const fontMono = style.getPropertyValue("--font-mono").trim() || "monospace";
  const monoSize = style.getPropertyValue("--font-size-mono").trim() || "12px";
  const captionSize = style.getPropertyValue("--font-size-caption").trim() || "11px";
  const accent = v("--accent");
  const separator = v("--separator");
  const theme: Partial<Theme> = {
    accentColor: accent,
    accentFg: v("--on-accent"),
    accentLight: v("--accent-soft"),
    textDark: text,
    textMedium: v("--text-secondary"),
    textLight: v("--text-tertiary"),
    textBubble: text,
    bgIconHeader: v("--text-secondary"),
    fgIconHeader: window,
    textHeader: v("--text-secondary"),
    textHeaderSelected: v("--on-accent"),
    bgCell: surface,
    bgCellMedium: over(v("--grid-stripe"), surface),
    bgHeader: window,
    bgHeaderHasFocus: over(v("--grid-hover"), window),
    bgHeaderHovered: over(v("--grid-hover"), window),
    bgBubble: over(v("--control"), surface),
    bgBubbleSelected: surface,
    bgSearchResult: v("--accent-faint"),
    borderColor: separator,
    horizontalBorderColor: "transparent",
    headerBottomBorderColor: v("--separator-strong"),
    drilldownBorder: separator,
    linkColor: v("--accent-text"),
    resizeIndicatorColor: accent,
    cellHorizontalPadding: 8,
    cellVerticalPadding: 3,
    headerIconSize: 16,
    fontFamily: fontMono,
    baseFontStyle: monoSize,
    headerFontStyle: `600 ${captionSize}`,
    markerFontStyle: captionSize,
    editorFontSize: monoSize,
    lineHeight: 1.4,
    roundingRadius: parseFloat(style.getPropertyValue("--radius-sm")) || 4,
  };
  return {
    theme,
    stripe: over(v("--grid-stripe"), surface),
    pending: over(v("--accent-soft"), surface),
    failed: over(v("--danger-soft"), surface),
    null: v("--null-text"),
    text,
    textSecondary: v("--text-secondary"),
    textTertiary: v("--text-tertiary"),
    accentText: v("--accent-text"),
    headerBg: window,
    separator,
    fontUI,
    fontMono,
    captionSize,
    monoSize,
  };
}
