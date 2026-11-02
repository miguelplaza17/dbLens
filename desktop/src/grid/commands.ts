// Actions the window's shortcuts and menus send to the grid on screen (save
// pending edits, Set NULL, copy, page up and down…). The mounted grid
// registers its handlers here; with no grid, nothing happens.

import type { Action } from "../shortcuts";
import { toggleHud } from "./perf";

export type GridCommand =
  | "saveChanges"
  | "revertChanges"
  | "setNull"
  | "copySelection"
  | "copyAll"
  | "freezeFirstColumn"
  | "pageUp"
  | "pageDown";

type Handler = (command: GridCommand) => boolean;

let handler: Handler | null = null;

/** The grid on screen takes the commands; returns a function that stops it. */
export function registerGrid(next: Handler): () => void {
  handler = next;
  return () => {
    if (handler === next) handler = null;
  };
}

/** Sends a command to the grid; false when no grid took it. */
export function gridCommand(command: GridCommand): boolean {
  return handler?.(command) ?? false;
}

/** The window's actions the grid takes (App.perform asks first); true when taken. */
export function gridAction(action: Action): boolean {
  switch (action) {
    case "perfHud":
      toggleHud();
      return true;
    case "copySelection":
      return gridCommand("copySelection");
    case "saveChanges":
      return gridCommand("saveChanges");
    case "revertChanges":
      return gridCommand("revertChanges");
    case "freezeFirstColumn":
      return gridCommand("freezeFirstColumn");
    case "copyRows":
      return gridCommand("copyAll");
    case "previousPage":
      return gridCommand("pageUp");
    case "nextPage":
      return gridCommand("pageDown");
    default:
      return false;
  }
}

/** The native menu's Set NULL sends this window event (src/native/perform.ts). */
export const SET_NULL_EVENT = "dblens:set-null";
