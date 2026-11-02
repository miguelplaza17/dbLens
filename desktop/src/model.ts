// The window's state and every action, in one place (AppModel.swift on macOS).
// Views read it through useModel() and call its methods; each change bumps a
// version so React redraws.

import { useSyncExternalStore } from "react";
import {
  api,
  errorText,
  mainResult,
  relationId,
  StreamCollector,
  type CellValue,
  type ColumnSort,
  type ConnectInfo,
  type Engine,
  type Execution,
  type Relation,
  type ResultBlock,
  type SavedConnection,
  type SshConfig,
  type StoreSnapshot,
} from "./api";
import type { MenuItem, MenuState } from "./components/Sheet";
import { TableSource } from "./grid/blocks";
import { markStart } from "./grid/perf";
import { duration, executionSummary } from "./format";
import { t } from "./i18n";

export const pageSizes = [50, 100, 200, 500, 1000];

export interface LogEntry {
  date: Date;
  isError: boolean;
  message: string;
  sql: string;
  /** Set when an all-or-nothing run was refused: the text to run plainly instead. */
  runAnyway?: string;
}

export type Sheet =
  | { kind: "connection"; editing: SavedConnection | null; url?: string }
  | { kind: "row"; row: number | null }
  | { kind: "shortcuts" }
  | {
      kind: "confirm";
      title: string;
      message: string;
      action: string;
      destructive: boolean;
      run: () => void | Promise<void>;
    };

export type Theme = "dark" | "light";

function stored(key: string, fallback: string): string {
  try {
    return localStorage.getItem(key) ?? fallback;
  } catch {
    return fallback;
  }
}

function store(key: string, value: string) {
  try {
    localStorage.setItem(key, value);
  } catch {
    // Private windows and previews: the setting lasts until the window closes.
  }
}

/** Kinds of relation that can take a new row. */
const insertable = new Set(["table", "partitioned", "foreign"]);

export class AppModel {
  // Saved connections
  store: StoreSnapshot = { connections: [], folders: [] };
  storeError: string | null = null;

  // The open connection
  session: ConnectInfo | null = null;
  /** The saved connection being opened, or "url" for a pasted one. */
  connecting: string | null = null;
  connectError: string | null = null;
  relations: Relation[] = [];
  relationsLoading = false;
  /** Sidebar schemas folded away, by schema name. */
  collapsedSchemas = new Set<string>();

  // The center pane
  shown: "table" | "query" = "table";
  relation: Relation | null = null;
  /** The table's first block (columns, keys, note). */
  tableResult: ResultBlock | null = null;
  /** The table's rows, read in blocks as the grid scrolls; holds pending edits. */
  tableSource: TableSource | null = null;
  tableError: string | null = null;
  tableLoading = false;
  /** Rows with the search; undefined while counting, null when too slow to count. */
  tableCount: number | null | undefined = undefined;
  offset = 0;
  pageSize = Number(stored("pageSize", "200")) || 200;
  tableSearch = "";
  tableSort: ColumnSort | null = null;
  /** Bumps when the grid shows new rows, so it drops its selection. */
  tableVersion = 0;

  // The SQL panel
  sql = "";
  sqlSelection = "";
  running = false;
  execution: Execution | null = null;
  querySort: ColumnSort | null = null;
  runVersion = 0;
  log: LogEntry | null = null;

  // Window
  showSQLPanel = stored("showSQLPanel", "1") === "1";
  showSidebar = true;
  sqlPanelHeight = Number(stored("sqlPanelHeight", "280")) || 280;
  theme: Theme = (new URLSearchParams(location.search).get("theme") as Theme | null) ?? (stored("theme", "dark") as Theme);
  sheet: Sheet | null = null;
  /** The grid's selected rows (indexes into the shown result). */
  selection = new Set<number>();
  /** A short message at the bottom of the window ("Copied 3 rows"). */
  toast: string | null = null;
  /** The open context menu. */
  menu: MenuState | null = null;

  private keyNoticeShown = false;
  private version = 0;
  private listeners = new Set<() => void>();
  private searchTimer: ReturnType<typeof setTimeout> | undefined;
  private toastTimer: ReturnType<typeof setTimeout> | undefined;
  private pageRequest = 0;

  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };
  getVersion = () => this.version;

  changed() {
    this.version++;
    for (const listener of this.listeners) listener();
  }

  // MARK: - Derived

  get engine(): Engine | null {
    return this.session?.engine ?? null;
  }
  /** Redis: the bottom panel runs commands. */
  get usesCommands(): boolean {
    return this.engine === "redis";
  }
  /** MongoDB: the bottom panel runs mongosh-style queries. */
  get usesQueries(): boolean {
    return this.engine === "mongodb";
  }
  get panelTitle(): string {
    return this.usesCommands ? t("Commands") : this.usesQueries ? t("Query") : "SQL";
  }
  get shownResult(): ResultBlock | null {
    if (this.shown === "table") return this.tableResult;
    return this.execution ? mainResult(this.execution) ?? null : null;
  }
  get canEditRows(): boolean {
    return this.shown === "table" && !!this.tableResult?.keys && this.relation?.kind !== "key";
  }
  get canInsertRows(): boolean {
    return this.shown === "table" && !!this.relation && insertable.has(this.relation.kind) && !!this.tableResult;
  }
  get canGoBack(): boolean {
    return this.shown === "table" && this.offset > 0 && !this.tableLoading;
  }
  get canGoForward(): boolean {
    if (this.shown !== "table" || !this.tableResult || this.tableLoading) return false;
    const end = this.offset + this.tableResult.rowCount;
    if (typeof this.tableCount === "number") return end < this.tableCount;
    return this.tableResult.rowCount >= this.pageSize;
  }
  get activeSavedId(): string | null {
    return this.session?.savedId ?? null;
  }

  // MARK: - Saved connections

  async loadStore() {
    try {
      this.store = await api.listConnections();
      this.storeError = null;
      if (this.store.keyInFile && !this.keyNoticeShown) {
        // Said once, in the log line, so people on a system without a keyring know.
        this.keyNoticeShown = true;
        if (!this.log) {
          this.log = {
            date: new Date(),
            isError: false,
            message: t("There is no system keyring here, so the key that seals saved passwords is kept in a file beside them."),
            sql: "",
          };
        }
      }
    } catch (e) {
      // A locked keyring, say: show it. The next action tries the store again.
      this.storeError = errorText(e);
      this.logError(this.storeError, "");
    }
    this.changed();
  }

  /** Opens a saved connection. */
  async connect(saved: SavedConnection) {
    await this.openSaved({ savedId: saved.id }, saved.id);
  }

  /** Connects first, then saves: only a URL that works is kept (ConnectionSheet.save).
   *  An edited saved connection is tried as typed (masked passwords filled in by
   *  the backend) and saved only once it connects. */
  async connectNew(draft: { url: string; name: string; ssh: SshConfig | null; editing: SavedConnection | null }) {
    const editing = draft.editing;
    await this.closeSession();
    this.connecting = editing ? editing.id : "url";
    this.connectError = null;
    this.changed();
    let info: ConnectInfo | null = null;
    try {
      info = editing
        ? await api.connect({ savedId: editing.id, url: draft.url, ssh: draft.ssh })
        : await api.connect({ url: draft.url, ssh: draft.ssh });
      const saved = await api.saveConnection(
        editing
          ? { id: editing.id, url: draft.url, name: draft.name, ssh: draft.ssh, folderId: editing.folderId }
          : { url: draft.url, name: draft.name, ssh: draft.ssh },
      );
      this.session = { ...info, savedId: saved.id, name: saved.name };
      this.connecting = null;
      await this.loadStore();
      this.opened();
    } catch (e) {
      // Connected but not saved: close that session (and its tunnel) instead of leaking it.
      if (info && this.session?.sessionId !== info.sessionId) await api.disconnect(info.sessionId).catch(() => undefined);
      this.connecting = null;
      this.changed();
      throw errorText(e);
    }
  }

  private async openSaved(target: { savedId: string }, savedId: string): Promise<boolean> {
    await this.closeSession();
    this.connecting = savedId;
    this.connectError = null;
    this.changed();
    try {
      this.session = await api.connect(target);
      this.connecting = null;
      this.opened();
      void this.loadStore();
      return true;
    } catch (e) {
      this.connecting = null;
      this.connectError = errorText(e);
      this.logError(this.connectError, "");
      this.changed();
      return false;
    }
  }

  /** A fresh connection: read its tables, say so in the log. */
  private opened() {
    const info = this.session;
    if (!info) return;
    this.sql = stored(`sql.${info.savedId ?? info.name}`, "");
    this.log = {
      date: new Date(),
      isError: false,
      message: `${info.name} · ${info.serverVersion}${info.sshLabel ? ` · SSH ${info.sshLabel}` : ""}`,
      sql: "",
    };
    this.changed();
    void this.loadRelations();
  }

  async loadRelations() {
    const info = this.session;
    if (!info) return;
    this.relationsLoading = true;
    this.changed();
    try {
      const relations = await api.relations(info.sessionId);
      if (this.session?.sessionId !== info.sessionId) return;
      this.relations = relations;
    } catch (e) {
      this.logError(errorText(e), "");
    }
    this.relationsLoading = false;
    this.changed();
  }

  private async closeSession() {
    // Pending grid edits are saved first, as when leaving their row.
    await this.flushEdits();
    const info = this.session;
    this.session = null;
    this.relations = [];
    this.relation = null;
    this.tableResult = null;
    this.tableSource?.close();
    this.tableSource = null;
    this.tableError = null;
    this.tableCount = undefined;
    this.execution = null;
    this.shown = "table";
    this.selection = new Set();
    this.running = false;
    this.changed();
    if (info) await api.disconnect(info.sessionId).catch(() => undefined);
  }

  async disconnect() {
    await this.closeSession();
    this.connectError = null;
    this.changed();
  }

  async deleteConnection(saved: SavedConnection) {
    if (this.activeSavedId === saved.id) await this.disconnect();
    await this.attempt(() => api.deleteConnection(saved.id));
    await this.loadStore();
  }

  async renameConnection(saved: SavedConnection, name: string) {
    if (!name.trim() || name === saved.name) return;
    await this.attempt(() => api.renameConnection(saved.id, name.trim()));
    if (this.session?.savedId === saved.id) this.session = { ...this.session, name: name.trim() };
    await this.loadStore();
  }

  async moveConnection(saved: SavedConnection, folderId: string) {
    await this.attempt(() => api.moveConnection(saved.id, folderId));
    await this.loadStore();
  }

  async addFolder(name: string) {
    const folder = await this.attempt(() => api.addFolder(name));
    await this.loadStore();
    return folder;
  }

  async renameFolder(id: string, name: string) {
    if (!name.trim()) return;
    await this.attempt(() => api.renameFolder(id, name.trim()));
    await this.loadStore();
  }

  async deleteFolder(id: string) {
    await this.attempt(() => api.deleteFolder(id));
    await this.loadStore();
  }

  async toggleFolder(id: string) {
    const folder = this.store.folders.find((f) => f.id === id);
    if (!folder) return;
    folder.expanded = !folder.expanded;
    this.changed();
    await this.attempt(() => api.setFolderExpanded(id, folder.expanded));
  }

  toggleSchema(schema: string) {
    if (this.collapsedSchemas.has(schema)) this.collapsedSchemas.delete(schema);
    else this.collapsedSchemas.add(schema);
    this.changed();
  }

  private async attempt<T>(work: () => Promise<T>): Promise<T | undefined> {
    try {
      return await work();
    } catch (e) {
      this.logError(errorText(e), "");
      this.changed();
      return undefined;
    }
  }

  // MARK: - Tables

  async openRelation(relation: Relation) {
    if (!this.session) return;
    const same = this.relation && relationId(this.relation) === relationId(relation);
    this.shown = "table";
    if (same && this.tableResult) {
      this.changed();
      return;
    }
    // Save pending edits while their table is still the open one.
    await this.flushEdits();
    this.relation = relation;
    this.offset = 0;
    this.tableSearch = "";
    this.tableSort = null;
    this.tableResult = null;
    this.tableError = null;
    this.tableCount = undefined;
    this.changed();
    await this.loadPage(true);
  }

  /** Reads the current page; `recount` also counts the rows (first page, new search). */
  async loadPage(recount = false) {
    const info = this.session;
    const relation = this.relation;
    if (!info || !relation) return;
    const request = ++this.pageRequest;
    this.tableLoading = true;
    this.changed();
    await this.flushEdits();
    if (this.tableSource?.hasPending) {
      // A row could not be saved: keep its edits (and the failed state) instead of reloading.
      this.tableLoading = false;
      this.changed();
      return;
    }
    try {
      markStart();
      const source = await TableSource.open(info.sessionId, relation, this.tableSearch, this.tableSort);
      if (request !== this.pageRequest) {
        source.close();
        return;
      }
      this.tableSource?.close();
      this.tableSource = source;
      this.tableResult = source.head ?? null;
      this.tableError = null;
      this.tableVersion++;
      // The selection belongs to the grid on screen; a reload behind a query result keeps it.
      if (this.shown === "table") this.selection = new Set();
    } catch (e) {
      if (request !== this.pageRequest) return;
      this.tableError = errorText(e);
      this.tableResult = null;
      this.tableSource?.close();
      this.tableSource = null;
    }
    this.tableLoading = false;
    this.changed();
    const source = this.tableSource;
    if (source && !recount && typeof this.tableCount === "number") source.setCount(this.tableCount);
    if (source && source.exact) {
      this.tableCount = source.rows;
      this.changed();
      return;
    }
    if (recount && !this.tableError) {
      this.tableCount = undefined;
      const search = this.tableSearch;
      try {
        const count = await api.count(info.sessionId, relation, search);
        if (request === this.pageRequest || (this.relation === relation && this.tableSearch === search)) {
          this.tableCount = count;
          this.tableSource?.setCount(count);
          this.changed();
        }
      } catch {
        this.tableCount = null;
        this.changed();
      }
    }
  }

  async reload() {
    if (!this.session) return;
    if (this.shown === "table" && this.relation) {
      await this.loadPage(true);
    }
    await this.loadRelations();
  }

  async nextPage() {
    if (!this.canGoForward) return;
    this.offset += this.pageSize;
    await this.loadPage();
  }

  async previousPage() {
    if (!this.canGoBack) return;
    this.offset = Math.max(0, this.offset - this.pageSize);
    await this.loadPage();
  }

  async setPageSize(size: number) {
    this.pageSize = size;
    store("pageSize", String(size));
    this.offset = 0;
    await this.loadPage();
  }

  /** Searches every page after a short pause in typing. */
  searchTable(text: string) {
    this.tableSearch = text;
    this.changed();
    clearTimeout(this.searchTimer);
    this.searchTimer = setTimeout(() => {
      this.offset = 0;
      void this.loadPage(true);
    }, 250);
  }

  async sort(sort: ColumnSort | null) {
    if (this.shown === "table") {
      this.tableSort = sort;
      this.offset = 0;
      await this.loadPage();
    } else {
      this.querySort = sort;
      this.runVersion++;
      this.changed();
    }
  }

  // MARK: - Edits

  private requireTable() {
    const info = this.session;
    const relation = this.relation;
    const result = this.tableResult;
    if (!info || !relation || !result) throw t("No table is open.");
    return { info, relation, result };
  }

  /** Saves one cell right away (Set to NULL in the menu, the row editor). */
  async updateCell(row: number, column: number, value: string | null) {
    const { result } = this.requireTable();
    await this.updateRow(row, [{ column: result.columns[column].name, value }]);
  }

  /** Sends only these changes for the row. Row keys are the backend's own exact
   *  form; when an edit can change the key (a key column, or Postgres's ctid and
   *  xmin, which every update renews) the row's block is read again. */
  async updateRow(row: number, changes: CellValue[]) {
    const { info, relation } = this.requireTable();
    const source = this.tableSource;
    if (!source?.editable) throw t("Read Only");
    if (changes.length === 0) return;
    const key = source.key(row) ?? (await source.keys([row]))[0];
    if (!key) throw t("Read Only");
    const outcome = await api.updateRow(info.sessionId, relation, key, changes);
    this.logOutcome(outcome.sql, outcome.command);
    // Other pending cells of the row stay pending; the saved ones are patched in.
    const rest = source.changes(row).filter((c) => !changes.some((d) => d.column === c.column && d.value === c.value));
    source.saved(row, changes);
    for (const c of rest) source.edit(row, source.columns.findIndex((col) => col.name === c.column), c.value);
    this.changed();
  }

  /** Saves the grid's pending edits of one row (one UPDATE with the changed columns). */
  async saveRow(row: number) {
    const source = this.tableSource;
    if (!source) return;
    const changes = source.changes(row);
    if (changes.length === 0) return;
    try {
      await this.updateRow(row, changes);
    } catch (e) {
      const message = errorText(e);
      source.failed(row, message);
      this.logError(message, "");
      this.showToast(message);
    }
  }

  /** Whether the grid holds edits not saved yet (the window asks before it closes). */
  get hasUnsavedEdits(): boolean {
    return !!this.tableSource?.hasPending;
  }

  /** Saves every pending edit; false when some row could not be saved (its error is shown). */
  async saveUnsavedEdits(): Promise<boolean> {
    await this.flushEdits();
    return !this.hasUnsavedEdits;
  }

  /** Saves every row with pending edits, before the rows are read anew. */
  private async flushEdits() {
    const source = this.tableSource;
    if (!source?.hasPending) return;
    for (const row of source.pendingRows()) await this.saveRow(row);
  }

  /** A row's cells (null when its block isn't loaded), for the row editor. */
  tableRow(row: number) {
    return this.tableSource?.rowCells(row) ?? null;
  }

  async insertRow(values: CellValue[]) {
    const { info, relation } = this.requireTable();
    const outcome = await api.insertRow(info.sessionId, relation, values);
    this.logOutcome(outcome.sql, outcome.command);
    await this.recount();
  }

  async deleteRows(rows: number[]) {
    const { info, relation } = this.requireTable();
    const source = this.tableSource;
    if (!source?.editable) throw t("Read Only");
    const keys = await source.keys(rows);
    const statements: string[] = [];
    try {
      for (const key of keys) {
        const outcome = await api.deleteRow(info.sessionId, relation, key);
        statements.push(outcome.sql);
      }
      this.logOutcome(statements.join(";\n"), statements.length === 1 ? "DELETE 1" : `DELETE ${statements.length}`);
    } catch (e) {
      this.logError(errorText(e), statements.join(";\n"));
    }
    this.selection = new Set();
    await this.recount();
  }

  /** Rows were added or removed: read the blocks on screen again and count anew. */
  private async recount() {
    const info = this.session;
    const relation = this.relation;
    const source = this.tableSource;
    if (!info || !relation || !source) return this.loadPage(true);
    await this.flushEdits();
    if (!source.hasPending) {
      source.revert();
      source.invalidate();
    }
    this.tableCount = undefined;
    this.changed();
    try {
      const count = await api.count(info.sessionId, relation, this.tableSearch);
      if (this.tableSource !== source) return;
      this.tableCount = count;
      source.setCount(count);
    } catch {
      this.tableCount = null;
    }
    this.changed();
  }

  private logOutcome(sql: string, command: string) {
    this.log = { date: new Date(), isError: false, message: command, sql };
  }

  private logError(message: string, sql: string) {
    this.log = { date: new Date(), isError: true, message, sql };
  }

  // MARK: - The SQL panel

  setSQL(text: string) {
    this.sql = text;
    if (this.session) store(`sql.${this.session.savedId ?? this.session.name}`, text);
  }

  /** The selection, or everything when nothing is selected. */
  private get runText(): string {
    return (this.sqlSelection.trim() ? this.sqlSelection : this.sql).trim();
  }

  async run(mode: "plain" | "rehearse" | "atomic" = "plain", textOverride?: string) {
    const info = this.session;
    const text = textOverride ?? this.runText;
    if (!info || !text || this.running) return;
    // Rehearsals and all-or-nothing runs are SQL only (not Redis commands or MongoDB queries).
    if ((this.usesCommands || this.usesQueries) && mode !== "plain") return;
    this.running = true;
    this.changed();
    markStart();
    const started = new Date();
    /** A streamed run already showed its first rows (and reset the grid). */
    let shownEarly = false;
    try {
      let execution: Execution;
      let note = "";
      if (mode === "rehearse") {
        execution = await api.rehearse(info.sessionId, text);
        note = t("Rehearsed: every change was rolled back.");
      } else if (mode === "atomic") {
        const outcome = await api.executeAtomically(info.sessionId, text);
        if (outcome.kind === "notAtomic") {
          this.log = { date: started, isError: true, message: outcome.message, sql: text, runAnyway: text };
          this.running = false;
          this.changed();
          return;
        }
        execution = outcome.execution;
        note = t("All statements committed together.");
      } else {
        // Streamed: the first rows show while the rest arrive.
        const collector = new StreamCollector();

        const done = await api.executeStream(info.sessionId, text, (message) => {
          collector.add(message);
          if (message.type === "rows" && !shownEarly && this.session?.sessionId === info.sessionId) {
            shownEarly = true;
            this.execution = collector.execution();
            this.querySort = null;
            this.runVersion++;
            this.shown = "query";
            this.selection = new Set();
            this.changed();
          } else if (shownEarly && (message.type === "rows" || message.type === "end")) {
            // New block objects (the column arrays are shared) so the grid redraws.
            const soFar = collector.execution();
            this.execution = { ...soFar, results: soFar.results.map((r) => ({ ...r })) };
            this.changed();
          }
        });
        if (done.error !== null) throw done.error;
        execution = collector.execution();
        if (done.cancelled) note = t("Stopped after %lld rows", mainResult(execution)?.rowCount ?? 0);
      }
      if (this.session?.sessionId !== info.sessionId) return;
      this.log = { date: started, isError: false, message: [executionSummary(execution), note].filter(Boolean).join(" · "), sql: text };
      const main = mainResult(execution);
      this.execution = execution;
      if (!shownEarly) {
        // Streamed rows are on screen already: keep the scroll position.
        this.querySort = null;
        this.runVersion++;
      }
      if (main && main.columns.length > 0) {
        if (!shownEarly) {
          this.shown = "query";
          this.selection = new Set();
        }
      } else if (mode !== "rehearse") {
        // A write: the open table and the list of tables may have changed.
        // Reload the open table even while a query result is shown, so the
        // Table tab never shows stale rows or keys.
        if (this.relation) void this.loadPage(true);
        if (/\b(create|drop|alter|rename)\b/i.test(text)) void this.loadRelations();
        if (!this.relation) this.shown = "query";
      }
    } catch (e) {
      this.log = { date: started, isError: true, message: errorText(e), sql: text };
    }
    this.running = false;
    this.changed();
  }

  cancel() {
    if (this.session && this.running) void api.cancel(this.session.sessionId);
  }

  /** Puts a query for the table in the editor (Query in SQL). */
  async queryTable(relation: Relation) {
    const info = this.session;
    if (!info) return;
    let text: string;
    if (this.usesCommands) {
      const verbs: Record<string, string> = { hash: "HGETALL", list: "LRANGE", set: "SMEMBERS", zset: "ZRANGE", stream: "XRANGE" };
      const verb = verbs[relation.subtype] ?? "GET";
      const extra = verb === "LRANGE" || verb === "ZRANGE" ? " 0 -1" : verb === "XRANGE" ? " - +" : "";
      text = `${verb} ${relation.name}${extra}`;
    } else if (this.usesQueries) {
      text = `db.${relation.name}.find({}).limit(100)`;
    } else {
      const name = await api.qualifiedName(info.sessionId, relation).catch(() => relationId(relation));
      text = `select * from ${name} limit 100;`;
    }
    this.setSQL(text);
    this.showSQLPanel = true;
    this.changed();
  }

  // MARK: - Window

  toggleSQLPanel() {
    this.showSQLPanel = !this.showSQLPanel;
    store("showSQLPanel", this.showSQLPanel ? "1" : "0");
    this.changed();
  }

  setSQLPanelHeight(height: number) {
    this.sqlPanelHeight = Math.round(height);
    store("sqlPanelHeight", String(this.sqlPanelHeight));
    this.changed();
  }

  toggleSidebar() {
    this.showSidebar = !this.showSidebar;
    this.changed();
  }

  toggleTheme() {
    this.theme = this.theme === "dark" ? "light" : "dark";
    store("theme", this.theme);
    this.changed();
  }

  open(sheet: Sheet | null) {
    this.sheet = sheet;
    this.changed();
  }

  setSelection(rows: Set<number>) {
    this.selection = rows;
    this.changed();
  }

  showMenu(event: { clientX: number; clientY: number; preventDefault(): void }, items: MenuItem[]) {
    event.preventDefault();
    this.menu = { x: event.clientX, y: event.clientY, items };
    this.changed();
  }

  closeMenu = () => {
    this.menu = null;
    this.changed();
  };

  showToast(message: string) {
    this.toast = message;
    this.changed();
    clearTimeout(this.toastTimer);
    this.toastTimer = setTimeout(() => {
      this.toast = null;
      this.changed();
    }, 1800);
  }

  showResultTab(which: "table" | "query") {
    this.shown = which;
    this.selection = new Set();
    this.changed();
  }

  /** "12 ms" for the last run, for the status line. */
  get runSummary(): string {
    return this.execution ? executionSummary(this.execution) : "";
  }

  get lastDuration(): string {
    return this.execution ? duration(this.execution.durationMs) : "";
  }
}

export const model = new AppModel();

/** Redraws the calling component whenever the model changes. */
export function useModel(): AppModel {
  useSyncExternalStore(model.subscribe, model.getVersion);
  return model;
}
