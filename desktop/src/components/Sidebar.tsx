// The sidebar: a filter, the saved connections (in folders), and the tables of
// the open connection grouped by schema (ContentView.swift).

import { forwardRef, useMemo, useState, type ReactNode } from "react";
import { api, engineNames, relationId, type Relation, type SavedConnection } from "../api";
import { t } from "../i18n";
import { useModel } from "../model";
import { withKeys } from "../shortcuts";
import { copyText } from "../format";
import { Icon, type IconName } from "./Icon";

const relationIcon: Record<Relation["kind"], IconName> = {
  table: "table",
  partitioned: "table",
  view: "view",
  materialized: "materialized",
  foreign: "foreign",
  key: "key",
};

export const Sidebar = forwardRef<HTMLInputElement>(function Sidebar(_props, filterRef) {
  const model = useModel();
  const [filter, setFilter] = useState("");
  const [renaming, setRenaming] = useState<string | null>(null);

  const needle = filter.trim().toLowerCase();
  const connections = model.store.connections;
  const matches = (c: SavedConnection) => !needle || c.name.toLowerCase().includes(needle);
  const byFolder = (folderId: string) =>
    connections.filter((c) => c.folderId === folderId && matches(c)).sort((a, b) => a.sortIndex - b.sortIndex);

  const groups = useMemo(() => {
    const map = new Map<string, Relation[]>();
    for (const r of model.relations) {
      if (needle && !r.name.toLowerCase().includes(needle) && !r.schema.toLowerCase().includes(needle)) continue;
      map.set(r.schema, [...(map.get(r.schema) ?? []), r]);
    }
    return [...map].sort(([a], [b]) => a.localeCompare(b));
  }, [model.relations, needle]);

  const filterPrompt = model.usesCommands ? t("Filter Keys") : model.usesQueries ? t("Filter Collections") : t("Filter Tables");

  const connectionMenu = (event: React.MouseEvent, c: SavedConnection) => {
    const active = model.activeSavedId === c.id;
    model.showMenu(event, [
      { title: t("Connect"), icon: "connect", action: () => void model.connect(c) },
      { title: t("Edit…"), icon: "edit", action: () => model.open({ kind: "connection", editing: c }) },
      { title: t("Rename…"), action: () => setRenaming(c.id) },
      ...(active ? [{ title: t("Disconnect"), icon: "disconnect" as IconName, action: () => void model.disconnect() }] : []),
      { separator: true, title: "" },
      ...model.store.folders
        .filter((f) => f.id !== c.folderId)
        .map((f) => ({ title: t("Move to %@", f.name), icon: "folder" as IconName, action: () => void model.moveConnection(c, f.id) })),
      ...(c.folderId ? [{ title: t("No Folder"), action: () => void model.moveConnection(c, "") }] : []),
      { separator: true, title: "" },
      {
        title: t("Delete…"),
        icon: "trash",
        destructive: true,
        action: () =>
          model.open({
            kind: "confirm",
            title: t("Delete “%@”?", c.name),
            message: t("The saved URL is erased. The database is not changed."),
            action: t("Delete"),
            destructive: true,
            run: () => model.deleteConnection(c),
          }),
      },
    ]);
  };

  const folderMenu = (event: React.MouseEvent, id: string) => {
    model.showMenu(event, [
      { title: t("Rename…"), icon: "edit", action: () => setRenaming(id) },
      { title: t("New Folder"), icon: "folderPlus", action: newFolder },
      { separator: true, title: "" },
      { title: t("Delete Folder"), icon: "trash", destructive: true, action: () => void model.deleteFolder(id) },
    ]);
  };

  const relationMenu = (event: React.MouseEvent, r: Relation) => {
    model.showMenu(event, [
      {
        title: model.usesCommands ? t("Inspect in Commands") : model.usesQueries ? t("Query in Panel") : t("Query in SQL"),
        icon: "terminal",
        action: () => void model.queryTable(r),
      },
      { title: model.usesCommands ? t("Copy Key") : t("Copy Name"), icon: "copy", action: () => void copyName(r) },
    ]);
  };

  const copyName = async (r: Relation) => {
    const info = model.session;
    if (!info) return;
    const name = model.usesCommands ? r.name : await api.qualifiedName(info.sessionId, r).catch(() => relationId(r));
    await copyText(name);
    model.showToast(t("Copied"));
  };

  async function newFolder() {
    const folder = await model.addFolder(t("New Folder"));
    if (folder) setRenaming(folder.id);
  }

  const row = (c: SavedConnection, nested: boolean) => {
    const active = model.activeSavedId === c.id;
    const busy = model.connecting === c.id;
    return (
      <SidebarRow
        key={c.id}
        icon="database"
        nested={nested}
        selected={active}
        title={c.name}
        renaming={renaming === c.id}
        onRename={(name) => {
          setRenaming(null);
          void model.renameConnection(c, name);
        }}
        tooltip={`${engineNames[c.engine]}${c.ssh ? ` · ${t("Via SSH %@", c.ssh.host)}` : ""}`}
        onClick={() => !active && !busy && void model.connect(c)}
        onContextMenu={(e) => connectionMenu(e, c)}
        trailing={busy ? <span className="spinner small" /> : active ? <span className="dot-active" aria-label={t("Connected")} /> : null}
      />
    );
  };

  return (
    <aside className="sidebar" aria-label={t("Sidebar")}>
      <div className="sidebar-filter">
        <Icon name="search" size={14} />
        <input
          ref={filterRef}
          type="search"
          value={filter}
          placeholder={filterPrompt}
          onChange={(e) => setFilter(e.target.value)}
          onKeyDown={(e) => e.key === "Escape" && setFilter("")}
          spellCheck={false}
        />
      </div>
      <div className="sidebar-scroll" onContextMenu={(e) => e.target === e.currentTarget && model.showMenu(e, [{ title: t("New Folder"), icon: "folderPlus", action: newFolder }])}>
        <div className="sidebar-section-header">
          <span>{t("Connections")}</span>
          <span className="sidebar-section-actions">
            <button className="icon-button" title={t("New Folder")} aria-label={t("New Folder")} onClick={newFolder}>
              <Icon name="folderPlus" size={14} />
            </button>
            <button
              className="icon-button"
              title={withKeys(t("New Connection"), "newConnection")}
              aria-label={t("New Connection…")}
              onClick={() => model.open({ kind: "connection", editing: null })}
            >
              <Icon name="plus" size={14} />
            </button>
          </span>
        </div>
        {byFolder("").map((c) => row(c, false))}
        {model.store.folders.map((folder) => {
          const inside = byFolder(folder.id);
          if (needle && inside.length === 0) return null;
          const open = folder.expanded || !!needle;
          return (
            <div key={folder.id}>
              <SidebarRow
                icon="folder"
                title={folder.name}
                disclosure={open ? "open" : "closed"}
                renaming={renaming === folder.id}
                onRename={(name) => {
                  setRenaming(null);
                  void model.renameFolder(folder.id, name);
                }}
                onClick={() => void model.toggleFolder(folder.id)}
                onContextMenu={(e) => folderMenu(e, folder.id)}
                trailing={!open && inside.length > 0 ? <span className="count">{inside.length}</span> : null}
              />
              {open && inside.map((c) => row(c, true))}
            </div>
          );
        })}
        <SidebarRow
          icon="plus"
          title={t("New Connection…")}
          muted
          onClick={() => model.open({ kind: "connection", editing: null })}
        />

        {model.session &&
          groups.map(([schema, relations]) => {
            const collapsed = model.collapsedSchemas.has(schema) && !needle;
            return (
              <div key={schema} className="sidebar-group">
                <button className="sidebar-section-header clickable" onClick={() => model.toggleSchema(schema)} aria-expanded={!collapsed}>
                  <span>{schema === "" ? t("keys") : schema}</span>
                  <Icon name={collapsed ? "chevronRight" : "chevronDown"} size={12} />
                </button>
                {!collapsed &&
                  relations.map((r) => (
                    <SidebarRow
                      key={relationId(r)}
                      icon={relationIcon[r.kind]}
                      title={r.name}
                      verbatim
                      selected={model.shown === "table" && !!model.relation && relationId(model.relation) === relationId(r)}
                      tooltip={r.subtype || r.kind}
                      onClick={() => void model.openRelation(r)}
                      onDoubleClick={() => void model.queryTable(r)}
                      onContextMenu={(e) => relationMenu(e, r)}
                    />
                  ))}
              </div>
            );
          })}
        {model.session && model.relationsLoading && model.relations.length === 0 && (
          <div className="sidebar-note">
            <span className="spinner small" />
          </div>
        )}
        {model.session && !model.relationsLoading && model.relations.length === 0 && (
          <div className="sidebar-note">{t("No tables")}</div>
        )}
      </div>
    </aside>
  );
});

function SidebarRow(props: {
  icon: IconName;
  title: string;
  nested?: boolean;
  selected?: boolean;
  muted?: boolean;
  verbatim?: boolean;
  tooltip?: string;
  disclosure?: "open" | "closed";
  trailing?: ReactNode;
  renaming?: boolean;
  onRename?: (name: string) => void;
  onClick?: () => void;
  onDoubleClick?: () => void;
  onContextMenu?: (event: React.MouseEvent) => void;
}) {
  const className = ["sidebar-row", props.nested && "nested", props.selected && "selected", props.muted && "muted"]
    .filter(Boolean)
    .join(" ");
  if (props.renaming) {
    return (
      <div className={className}>
        <Icon name={props.icon} size={15} />
        <input
          className="rename-field"
          autoFocus
          defaultValue={props.title}
          onFocus={(e) => e.currentTarget.select()}
          onBlur={(e) => props.onRename?.(e.currentTarget.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") props.onRename?.(e.currentTarget.value);
            if (e.key === "Escape") props.onRename?.(props.title);
          }}
        />
      </div>
    );
  }
  return (
    <button
      className={className}
      title={props.tooltip}
      onClick={props.onClick}
      onDoubleClick={props.onDoubleClick}
      onContextMenu={props.onContextMenu}
      aria-current={props.selected ? "true" : undefined}
    >
      {props.disclosure && <Icon name={props.disclosure === "open" ? "chevronDown" : "chevronRight"} size={12} />}
      <Icon name={props.icon} size={15} />
      <span className="sidebar-row-title">{props.title}</span>
      {props.trailing}
    </button>
  );
}
