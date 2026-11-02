// One dispatcher for every action, whether it came from a key press (App.tsx)
// or a native menu click (bridge.ts). The same shortcut can reach both: a
// menu accelerator and the page's key handler. A repeat of the same action
// within a few milliseconds is the same press, so it runs once.

import type { Action } from "../shortcuts";

export type Performer = (action: Action) => void;

let performer: Performer | null = null;
let last: { action: Action; at: number } | null = null;

/** The window's action handler; the latest registration wins. */
export function setPerformer(fn: Performer | null) {
  performer = fn;
}

export const DEDUPE_MS = 120;

/** Runs the action. Returns false when it was dropped as a duplicate or nothing handles it. */
export function dispatch(action: Action, now: number = performance.now()): boolean {
  if (!performer) return false;
  if (last && last.action === action && now - last.at < DEDUPE_MS) return false;
  last = { action, at: now };
  performer(action);
  return true;
}

/** Test hook: forget the last action. */
export function resetDispatcher() {
  last = null;
  performer = null;
}
