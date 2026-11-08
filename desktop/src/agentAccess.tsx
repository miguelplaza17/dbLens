// Agent Tools: what agents using dbLens's MCP server (src-tauri/src/mcp.rs) may
// do on one connection — seen or hidden, each tool on or off, and writing.
// In the main window's Agent Tools sheet and in Settings > Agents
// (AgentAccessItems in AgentSettings.swift).

import { useEffect, useState } from "react";
import type { SavedConnection } from "./api";
import { t } from "./i18n";

/** mcp.rs CONNECTION_TOOLS: the tools that work on one connection. */
export const READING_TOOLS = ["list_tables", "describe_schema", "describe_table", "list_extensions", "query"];
export const WRITING_TOOLS = ["test_changes", "execute", "install_extension"];
export const CONNECTION_TOOLS = [...READING_TOOLS, ...WRITING_TOOLS];

export interface AgentAccess {
  hidden: boolean;
  blockedTools: string[];
  canWrite: boolean;
  /** What agents should know about this database; sent back with every answer from it. */
  notes: string;
}

export function accessOf(c: SavedConnection): AgentAccess {
  return { hidden: !!c.agentsHidden, blockedTools: c.agentsBlockedTools ?? [], canWrite: !!c.agentsCanWrite, notes: c.agentsNotes ?? "" };
}

/** Allowed unless turned off: tools added later start allowed. */
export function mayUse(access: AgentAccess, tool: string): boolean {
  return !access.hidden && !access.blockedTools.includes(tool);
}

/** "Hidden", "Read only", "Reads and writes", or "3 of 5 tools" when some are off. */
export function accessSummary(c: SavedConnection): string {
  const access = accessOf(c);
  if (access.hidden) return t("Hidden");
  const offered = access.canWrite ? CONNECTION_TOOLS : READING_TOOLS;
  const allowed = offered.filter((tool) => mayUse(access, tool)).length;
  if (allowed === 0) return t("No tools");
  if (allowed < offered.length) return t("%@ of %@ tools", allowed, offered.length);
  return access.canWrite ? t("Reads and writes") : t("Read only");
}

/** The switches; `onChange` gets the whole new access. */
export function AgentAccessEditor({ connection, onChange }: { connection: SavedConnection; onChange: (access: AgentAccess) => void }) {
  const access = accessOf(connection);
  const setTool = (tool: string, on: boolean) => {
    const blocked = access.blockedTools.filter((x) => x !== tool);
    onChange({ ...access, blockedTools: on ? blocked : [...blocked, tool].sort() });
  };
  // Typed locally and saved when the field loses focus, not on every key.
  const [notes, setNotes] = useState(access.notes);
  useEffect(() => setNotes(access.notes), [access.notes]);
  const row = (key: string, label: string, checked: boolean, disabled: boolean, change: (on: boolean) => void, code = false) => (
    <label key={key} className="form-row toggle-row">
      <span className="form-label wide">{code ? <code className="settings-code">{label}</code> : label}</span>
      <input type="checkbox" className="switch" checked={checked} disabled={disabled} onChange={(e) => change(e.target.checked)} />
    </label>
  );
  return (
    <>
      <label className="form-row notes-row">
        <span className="form-label wide">{t("Notes for Agents")}</span>
        <textarea
          className="notes-field"
          rows={3}
          value={notes}
          placeholder={t("What it holds, naming conventions, tables to avoid")}
          onChange={(e) => setNotes(e.target.value)}
          onBlur={() => notes.trim() !== access.notes.trim() && onChange({ ...access, notes: notes.trim() })}
        />
      </label>
      {row("visible", t("Visible to Agents"), !access.hidden, false, (on) => onChange({ ...access, hidden: !on }))}
      {READING_TOOLS.map((tool) => row(tool, tool, mayUse(access, tool), access.hidden, (on) => setTool(tool, on), true))}
      {row("write", t("Let Agents Write"), access.canWrite, access.hidden, (on) => onChange({ ...access, canWrite: on }))}
      {WRITING_TOOLS.map((tool) =>
        row(tool, tool, mayUse(access, tool), access.hidden || !access.canWrite, (on) => setTool(tool, on), true),
      )}
    </>
  );
}
