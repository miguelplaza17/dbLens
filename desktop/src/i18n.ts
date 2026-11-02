// UI text: English is the source; each key is the exact English text, shared
// with the macOS app's Localization/<lang>.json where the text is the same.
// src/i18n/en.json lists every key (key → same text); src/i18n/pt-BR.json has
// the Portuguese. Interpolations use the Swift format specifiers in keys
// (%@ text, %lld number; %1$@ positional in values).
// Database data (names, SQL, values, server messages) is never translated:
// render it directly, never through t().

import en from "./i18n/en.json";
import ptBR from "./i18n/pt-BR.json";

type Table = Record<string, string>;

const tables: Record<string, Table> = { en: en as Table, "pt-BR": ptBR as Table, pt: ptBR as Table };

function preferred(): string[] {
  const forced = typeof location === "undefined" ? null : new URLSearchParams(location.search).get("lang");
  if (forced) return [forced];
  if (typeof navigator === "undefined") return [];
  return [...(navigator.languages ?? []), navigator.language].filter(Boolean);
}

function pick(): { code: string; table: Table | undefined } {
  for (const lang of preferred()) {
    if (lang.toLowerCase().startsWith("en")) return { code: "en", table: undefined };
    const table = tables[lang] ?? tables[lang.split("-")[0]];
    if (table) return { code: lang, table };
  }
  return { code: "en", table: undefined };
}

const chosen = pick();

/** The UI language, for number and date formats. */
export const language = chosen.code;

/** The current language's text for an English key, with arguments filled in. */
export function t(key: string, ...args: Array<string | number>): string {
  const text = chosen.table?.[key] ?? key;
  let next = 0;
  return text.replace(/%(?:(\d+)\$)?(@|lld|d)/g, (_match, position: string | undefined) => {
    const index = position ? Number(position) - 1 : next++;
    const value = args[index];
    return value === undefined ? "" : typeof value === "number" ? formatNumber(value) : value;
  });
}

const numbers = new Intl.NumberFormat(language);

/** 12,345 in English, 12.345 in Portuguese. */
export function formatNumber(value: number): string {
  return numbers.format(value);
}
