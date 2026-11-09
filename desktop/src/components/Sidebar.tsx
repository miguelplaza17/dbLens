// The sidebar: a filter, the saved connections (in folders), and the tables of
// the open connection grouped by schema (ContentView.swift).

import { Fragment, forwardRef, useMemo, useState, type CSSProperties, type ReactNode } from "react";
import { EngineIcon } from "../engineIcons";
import { api, engineNames, relationId, type ConnectionFolder, type Relation, type SavedConnection } from "../api";
import { t } from "../i18n";
import { useModel } from "../model";
import { withKeys } from "../shortcuts";
import { copyText } from "../format";
import { beginConnectionDrag, beginFolderDrag } from "../native/dragConnections";
import { copyUrl, copyUrlWithPassword, openAgentTools, openInTerminal } from "../history/connectionActions";
import { accessSummary } from "../agentAccess";
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
  const folders = model.store.folders;
  const folderIds = new Set(folders.map((f) => f.id));
  /** The folders right inside `parent` ("" is the top level); one whose parent is gone shows at the top. */
  const subfolders = (parent: string) => folders.filter((f) => (folderIds.has(f.parentId) ? f.parentId : "") === parent);
  /** While filtering, a folder shows when something inside it, at any depth, matches. */
  const hasMatch = (id: string, seen = new Set<string>()): boolean =>
    !seen.has(id) && (seen.add(id), byFolder(id).length > 0 || subfolders(id).some((f) => hasMatch(f.id, seen)));

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
      { title: t("Open in Terminal"), icon: "terminal", action: () => void openInTerminal(model, c) },
      { title: t("Copy URL Without Password"), icon: "copy", action: () => void copyUrl(model, c) },
      { title: t("Copy URL with Password…"), action: () => copyUrlWithPassword(model, c) },
      {
        title: c.agentsCanWrite ? t("Stop Letting Agents Write") : t("Let Agents Write"),
        icon: "terminal",
        action: () => void model.setAgentsCanWrite(c, !c.agentsCanWrite),
      },
      { title: t("Agent Tools…"), detail: accessSummary(c), action: () => openAgentTools(model, c) },
      ...(active ? [{ title: t("Disconnect"), icon: "disconnect" as IconName, action: () => void model.disconnect() }] : []),
      { separator: true, title: "" },
      ...folders
        .filter((f) => f.id !== c.folderId)
        .map((f) => ({ title: t("Move to %@", model.folderPath(f.id)), icon: "folder" as IconName, action: () => void model.moveConnection(c, f.id) })),
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

  const folderMenu = (event: React.MouseEvent, folder: ConnectionFolder) => {
    const id = folder.id;
    model.showMenu(event, [
      { title: t("Rename…"), icon: "edit", action: () => setRenaming(id) },
      { title: t("New Subfolder"), icon: "folderPlus", action: () => void newFolder(id) },
      { title: t("New Folder"), icon: "folderPlus", action: () => void newFolder() },
      { separator: true, title: "" },
      // Anywhere but inside itself.
      ...folders
        .filter((f) => f.id !== folder.parentId && !model.folderIsIn(f.id, id))
        .map((f) => ({ title: t("Move to %@", model.folderPath(f.id)), icon: "folder" as IconName, action: () => void model.moveFolder(id, f.id) })),
      ...(folder.parentId ? [{ title: t("Move to %@", t("Top Level")), action: () => void model.moveFolder(id, "") }] : []),
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

  async function newFolder(parentId = "") {
    const folder = await model.addFolder(t("New Folder"), parentId);
    if (folder) setRenaming(folder.id);
  }

  /** A folder, then (when open) its connections and its subfolders, each a level in. */
  const folderTree = (folder: ConnectionFolder, depth: number): ReactNode => {
    if (needle && !hasMatch(folder.id)) return null;
    const inside = byFolder(folder.id);
    const open = folder.expanded || !!needle;
    return (
      <div key={folder.id}>
        <DropSlot slot={`before:${folder.id}`} depth={depth} />
        <SidebarRow
          icon="folder"
          title={folder.name}
          depth={depth}
          disclosure={open ? "open" : "closed"}
          renaming={renaming === folder.id}
          onRename={(name) => {
            setRenaming(null);
            void model.renameFolder(folder.id, name);
          }}
          onClick={() => void model.toggleFolder(folder.id)}
          onContextMenu={(e) => folderMenu(e, folder)}
          // Onto another folder: inside it; onto its top edge: in front of it.
          onMouseDown={(e) =>
            beginFolderDrag(e, {
              allowed: (spot) => spot.before !== folder.id && !(spot.folder && model.folderIsIn(spot.folder, folder.id)),
              onDrop: (spot) => model.moveFolder(folder.id, spot.folder, spot.before),
              onSpring: (id) => void model.openFolder(id),
            })
          }
          dropFolder={folder.id}
          folderParent={folder.parentId}
          trailing={!open && inside.length > 0 ? <span className="count">{inside.length}</span> : null}
        />
        {open && inside.map((c) => row(c, depth + 1))}
        {open && <DropSlot slot={`connections:${folder.id}`} depth={depth + 1} />}
        {open && subfolders(folder.id).map((f) => folderTree(f, depth + 1))}
        {open && <DropSlot slot={`folders:${folder.id}`} depth={depth + 1} />}
      </div>
    );
  };

  const row = (c: SavedConnection, depth: number) => {
    const active = model.activeSavedId === c.id;
    const busy = model.connecting === c.id;
    return (
      <Fragment key={c.id}>
      <DropSlot slot={`before:${c.id}`} depth={depth} />
      <SidebarRow
        icon="database"
        iconNode={<EngineIcon engine={c.engine} />}
        depth={depth}
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
        onMouseDown={(e) =>
          beginConnectionDrag(e, {
            allowed: (spot) => spot.row !== c.id,
            onDrop: (spot) => model.moveConnection(c, spot.folder, spot.row),
            onSpring: (id) => void model.openFolder(id),
          })
        }
        dropRow={c.id}
        rowFolder={c.folderId}
        trailing={busy ? <span className="spinner small" /> : active ? <span className="dot-active" aria-label={t("Connected")} /> : null}
      />
      </Fragment>
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
      <div className="sidebar-scroll" onContextMenu={(e) => e.target === e.currentTarget && model.showMenu(e, [{ title: t("New Folder"), icon: "folderPlus", action: () => void newFolder() }])}>
        <div className="sidebar-section-header" data-drop-folder="">
          <span>{t("Connections")}</span>
          <span className="sidebar-section-actions">
            <button className="icon-button" title={t("New Folder")} aria-label={t("New Folder")} onClick={() => void newFolder()}>
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
        {byFolder("").map((c) => row(c, 0))}
        <DropSlot slot="connections:" depth={0} />
        {subfolders("").map((folder) => folderTree(folder, 0))}
        <DropSlot slot="folders:" depth={0} />

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

/** Empty until a drag can land here; then it opens (native/dragConnections.ts). */
function DropSlot(props: { slot: string; depth: number }) {
  return <div className="drop-slot" data-slot={props.slot} style={{ "--depth": props.depth } as CSSProperties} aria-hidden="true" />;
}

function SidebarRow(props: {
  icon: IconName;
  /** Drawn instead of `icon` (a kind of database's own icon). */
  iconNode?: ReactNode;
  title: string;
  /** Folder levels in: 0 at the top level. */
  depth?: number;
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
  onMouseDown?: (event: React.MouseEvent) => void;
  /** Dropping a connection here moves it into this folder. */
  dropFolder?: string;
  /** A folder row's parent ("" at the top level): a folder dropped on its top edge lands there. */
  folderParent?: string;
  /** A connection row: a dragged connection lands in front of it, in `rowFolder`. */
  dropRow?: string;
  rowFolder?: string;
}) {
  const depth = props.depth ?? 0;
  const style = depth > 0 ? ({ "--depth": depth } as CSSProperties) : undefined;
  const className = ["sidebar-row", depth > 0 && "nested", props.selected && "selected", props.muted && "muted"]
    .filter(Boolean)
    .join(" ");
  if (props.renaming) {
    return (
      <div className={className} style={style}>
        {props.iconNode ?? <Icon name={props.icon} size={15} />}
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
      onMouseDown={props.onMouseDown}
      data-drop-folder={props.dropFolder}
      data-folder-parent={props.folderParent}
      data-open={props.disclosure ? String(props.disclosure === "open") : undefined}
      data-drop-row={props.dropRow}
      data-row-folder={props.rowFolder}
      style={style}
      aria-current={props.selected ? "true" : undefined}
    >
      {props.disclosure && <Icon name={props.disclosure === "open" ? "chevronDown" : "chevronRight"} size={12} />}
      {props.iconNode ?? <Icon name={props.icon} size={15} />}
      <span className="sidebar-row-title">{props.title}</span>
      {props.trailing}
    </button>
  );
}
