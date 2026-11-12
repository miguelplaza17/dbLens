// The Settings window (Sources/dbLens/SettingsView.swift): a sidebar of topics
// and one grouped form per topic. It runs in its own Tauri window (label
// "settings", opened by Settings… / Mod+,) and sizes itself to its content.
// Settings live in Rust (src-tauri/src/settings.rs); every change reaches all
// windows at once.

import { Fragment, useCallback, useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { api, engineNames, inTauri, type AgentClientId, type AgentStatus, type AgentTool, type Engine, type SavedConnection, type StoreSnapshot } from "../api";
import { EngineIcon, engineDefaultColors, type EngineIconPref } from "../engineIcons";
import { copyText } from "../format";
import { language, t } from "../i18n";
import { AgentAccessEditor, accessOf, accessSummary, type AgentAccess } from "../agentAccess";
import { Empty } from "../components/Empty";
import { Icon, type IconName } from "../components/Icon";
import { ACCENTS } from "./accents";
import "./settings.css";
import {
  fitSettings,
  followSettings,
  setSettings,
  settingsInfo,
  showDataFolder,
  type Appearance,
  type Settings,
  type SettingsInfo,
} from "./store";

export type SettingsTab = "general" | "appearance" | "connections" | "agents" | "updates" | "data";

const TABS: { id: SettingsTab; title: string; icon: IconName }[] = [
  { id: "general", title: "General", icon: "more" },
  { id: "appearance", title: "Appearance", icon: "sun" },
  { id: "connections", title: "Connections", icon: "database" },
  { id: "agents", title: "Agents", icon: "terminal" },
  { id: "updates", title: "Updates", icon: "refresh" },
  { id: "data", title: "Data & Privacy", icon: "key" },
];

const TAB_KEY = "dblens.settingsTab";

function firstTab(): SettingsTab {
  const asked = new URLSearchParams(location.search).get("tab");
  let saved: string | null = null;
  try {
    saved = localStorage.getItem(TAB_KEY);
  } catch {
    // No storage: start at the first tab.
  }
  const id = asked ?? saved;
  return TABS.some((tab) => tab.id === id) ? (id as SettingsTab) : "general";
}

export default function SettingsApp() {
  const [settings, setLocal] = useState<Settings | null>(null);
  const [info, setInfo] = useState<SettingsInfo | null>(null);
  const [tab, setTab] = useState<SettingsTab>(firstTab);
  const [notice, setNotice] = useState<string | null>(null);
  const noticeTimer = useRef<ReturnType<typeof setTimeout>>();
  const sidebarRef = useRef<HTMLDivElement>(null);
  const contentRef = useRef<HTMLDivElement>(null);

  useLayoutEffect(() => {
    const forced = new URLSearchParams(location.search).get("theme");
    if (forced) document.documentElement.dataset.theme = forced;
    return followSettings((theme) => (document.documentElement.dataset.theme = theme), setLocal);
  }, []);

  useEffect(() => {
    void settingsInfo().then(setInfo);
    document.title = t("Settings");
  }, []);

  // Mod+W closes the window (it has no menu bar on Windows and Linux).
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "w") {
        event.preventDefault();
        if (inTauri) void getCurrentWindow().close();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  // The window is as tall as the tab needs: no scroll bar with nothing to scroll.
  useLayoutEffect(() => {
    const measure = () => {
      const height = Math.max(sidebarRef.current?.offsetHeight ?? 0, contentRef.current?.offsetHeight ?? 0);
      if (height > 0) void fitSettings(height);
    };
    measure();
    const observer = new ResizeObserver(measure);
    if (sidebarRef.current) observer.observe(sidebarRef.current);
    if (contentRef.current) observer.observe(contentRef.current);
    return () => observer.disconnect();
  }, [tab, settings === null]);

  const choose = (next: SettingsTab) => {
    setTab(next);
    try {
      localStorage.setItem(TAB_KEY, next);
    } catch {
      // Remembering the tab is a convenience.
    }
  };

  const change = useCallback((patch: Partial<Settings>) => {
    setLocal((current) => (current ? { ...current, ...patch } : current));
    void setSettings(patch).then(setLocal, (error) => say(String(error)));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const say = (text: string) => {
    setNotice(text);
    clearTimeout(noticeTimer.current);
    noticeTimer.current = setTimeout(() => setNotice(null), 2400);
  };

  const copy = (text: string) => void copyText(text).then(() => say(t("Copied")));

  const current = TABS.find((x) => x.id === tab) ?? TABS[0];

  return (
    <div className="settings">
      <nav className="settings-sidebar" aria-label={t("Settings")}>
        <div ref={sidebarRef} className="settings-sidebar-list" role="tablist" aria-orientation="vertical">
          {TABS.map((x) => (
            <button
              key={x.id}
              type="button"
              role="tab"
              aria-selected={x.id === tab}
              className={`settings-tab${x.id === tab ? " selected" : ""}`}
              onClick={() => choose(x.id)}
            >
              <span className="settings-tab-icon">
                <Icon name={x.icon} size={14} />
              </span>
              {t(x.title)}
            </button>
          ))}
        </div>
      </nav>
      <main className="settings-pane" role="tabpanel" aria-label={t(current.title)}>
        <div ref={contentRef} className="settings-content">
          <h1 className="settings-title">{t(current.title)}</h1>
          {settings && tab === "general" && <GeneralTab settings={settings} change={change} info={info} />}
          {settings && tab === "appearance" && <AppearanceTab settings={settings} change={change} />}
          {tab === "connections" && <ConnectionsTab say={say} />}
          {tab === "agents" && <AgentsTab info={info} copy={copy} say={say} />}
          {settings && tab === "updates" && <UpdatesTab settings={settings} change={change} info={info} say={say} />}
          {tab === "data" && <DataTab info={info} say={say} />}
        </div>
      </main>
      {notice && (
        <div className="settings-notice" role="status">
          {notice}
        </div>
      )}
    </div>
  );
}

// MARK: - Building blocks (the same grouped form as the connection sheet)

function Group({ title, footer, children }: { title?: string; footer?: ReactNode; children: ReactNode }) {
  return (
    <section className="form-group">
      {title && <header className="form-group-header">{title}</header>}
      <div className="form-card">{children}</div>
      {footer && <p className="form-footer">{footer}</p>}
    </section>
  );
}

/** A row: a label (with an optional second line) and its control on the right. */
function Row({ label, detail, children, htmlFor }: { label: ReactNode; detail?: ReactNode; children?: ReactNode; htmlFor?: string }) {
  return (
    <div className="form-row settings-row">
      <label className="settings-row-label" htmlFor={htmlFor}>
        <span>{label}</span>
        {detail && <span className="settings-row-detail">{detail}</span>}
      </label>
      {children && <div className="settings-row-control">{children}</div>}
    </div>
  );
}

function Toggle({ id, label, detail, checked, disabled, onChange }: {
  id: string;
  label: string;
  detail?: string;
  checked: boolean;
  disabled?: boolean;
  onChange: (on: boolean) => void;
}) {
  return (
    <Row label={label} detail={detail} htmlFor={id}>
      <input
        id={id}
        type="checkbox"
        role="switch"
        className="switch"
        checked={checked}
        disabled={disabled}
        onChange={(e) => onChange(e.target.checked)}
      />
    </Row>
  );
}

type Change = (patch: Partial<Settings>) => void;

const engineKinds: Engine[] = ["postgres", "mysql", "sqlite", "redis", "mongodb"];

// MARK: - Appearance (AppearanceSettings.swift, Theme.swift)

function GeneralTab({ settings, change, info }: { settings: Settings; change: Change; info: SettingsInfo | null }) {
  return (
    <>
      <Group
        footer={t("Closing the last window leaves dbLens running; open it again from its icon. Quit from the icon's menu or the File menu.")}
      >
        <Toggle
          id="keep-in-tray"
          label={info?.os === "macos" ? t("Keep dbLens in the menu bar") : t("Keep dbLens in the tray")}
          checked={settings.keepInTray}
          onChange={(on) => change({ keepInTray: on })}
        />
      </Group>
      <Group title={t("SQL Panel")}>
        <Toggle id="show-sql-panel" label={t("Show the SQL panel")} checked={settings.showSQLPanel} onChange={(on) => change({ showSQLPanel: on })} />
      </Group>
      <Group title={t("Connections")}>
        <Toggle id="confirm-delete" label={t("Ask before deleting a connection")} checked={settings.confirmDelete} onChange={(on) => change({ confirmDelete: on })} />
      </Group>
    </>
  );
}

function AppearanceTab({ settings, change }: { settings: Settings; change: Change }) {
  const modes: { id: Appearance; title: string }[] = [
    { id: "system", title: "System" },
    { id: "light", title: "Light" },
    { id: "dark", title: "Dark" },
  ];
  const accent = ACCENTS.find((a) => a.id === settings.accent) ?? ACCENTS[0];
  return (
    <>
      <Group footer={t("System follows the light or dark mode of your computer.")}>
        <Row label={t("Appearance")}>
          <div className="segmented" role="radiogroup" aria-label={t("Appearance")}>
            {modes.map((m) => (
              <button
                key={m.id}
                type="button"
                role="radio"
                aria-checked={settings.appearance === m.id}
                className={settings.appearance === m.id ? "selected" : ""}
                onClick={() => change({ appearance: m.id })}
              >
                {t(m.title)}
              </button>
            ))}
          </div>
        </Row>
        <Row label={t("Accent Color")}>
          <div className="accent-picker">
            <div className="accent-swatches" role="radiogroup" aria-label={t("Accent Color")}>
              {ACCENTS.map((a) => (
                <button
                  key={a.id}
                  type="button"
                  role="radio"
                  aria-checked={a.id === settings.accent}
                  aria-label={t(a.name)}
                  title={t(a.name)}
                  className={`accent-swatch swatch-${a.id}${a.id === settings.accent ? " selected" : ""}`}
                  onClick={() => change({ accent: a.id })}
                />
              ))}
            </div>
            <span className="settings-row-detail">{t(accent.name)}</span>
          </div>
        </Row>
      </Group>
      <Group title={t("Database Icons")} footer={t("The logos belong to their owners.")}>
        {engineKinds.map((e) => {
          const pref = settings.engineIcons[e];
          const style = pref?.style ?? "logo";
          const color = pref?.color ?? engineDefaultColors[e];
          const set = (next: Partial<EngineIconPref>) =>
            change({ engineIcons: { ...settings.engineIcons, [e]: { style, color: pref?.color, ...next } } });
          return (
            <Row key={e} label={<span className="engine-label"><EngineIcon engine={e} />{engineNames[e]}</span>}>
              <div className="segmented" role="radiogroup" aria-label={t("Icon")}>
                {(["logo", "symbol"] as const).map((s) => (
                  <button key={s} type="button" role="radio" aria-checked={style === s} className={style === s ? "selected" : ""} onClick={() => set({ style: s })}>
                    {s === "logo" ? t("Logo") : t("Symbol")}
                  </button>
                ))}
              </div>
              <input type="color" className="color-well" aria-label={t("Color")} value={color} onChange={(ev) => set({ color: ev.target.value })} />
              <button
                type="button"
                className="button"
                disabled={!pref}
                onClick={() => {
                  const { [e]: _removed, ...rest } = settings.engineIcons;
                  change({ engineIcons: rest });
                }}
              >
                {t("Reset")}
              </button>
            </Row>
          );
        })}
      </Group>
      <Group title={t("Preview")}>
        <div className="form-row settings-preview">
          <button type="button" className="button prominent" tabIndex={-1}>
            {t("Run")}
          </button>
          <button type="button" className="button" tabIndex={-1}>
            {t("Cancel")}
          </button>
          <span className="settings-preview-link">{t("Accent-colored text")}</span>
          <span className="engine-tag">PostgreSQL</span>
          <input type="checkbox" className="switch" checked readOnly tabIndex={-1} aria-hidden />
        </div>
      </Group>
    </>
  );
}

// MARK: - Connections (ConnectionSettings in SettingsView.swift)

function ConnectionsTab({ say }: { say: (text: string) => void }) {
  const [snapshot, setSnapshot] = useState<StoreSnapshot | null>(null);
  const load = useCallback(() => void api.listConnections().then(setSnapshot, (e) => say(String(e))), [say]);

  useEffect(() => {
    load();
    // Connections change in the main window; show them when this one comes back.
    window.addEventListener("focus", load);
    return () => window.removeEventListener("focus", load);
  }, [load]);

  if (!snapshot) return null;
  if (snapshot.connections.length === 0) {
    return (
      <div className="settings-empty">
        <Empty icon="database" title={t("No saved connections yet.")}>
          <p>{t("New connections and deleting are in the main window's sidebar.")}</p>
        </Empty>
      </div>
    );
  }
  const folders = new Map(snapshot.folders.map((f) => [f.id, f.name]));
  const clear = (connection: SavedConnection) => {
    void api.clearSqlHistory(connection.id).then(
      () => say(t("Cleared the history of %@", connection.name)),
      (e) => say(String(e)),
    );
  };
  const sorted = [...snapshot.connections].sort((a, b) => a.name.localeCompare(b.name));
  return (
    <Group footer={t("New connections and deleting are in the main window's sidebar.")}>
      {sorted.map((c) => {
        const parts = [engineNames[c.engine]];
        if (c.folderId && folders.has(c.folderId)) parts.push(folders.get(c.folderId) as string);
        if (c.ssh) parts.push(t("via SSH %@", c.ssh.host));
        return (
          <Row key={c.id} label={<span className="selectable">{c.name}</span>} detail={parts.join(" · ")}>
            <button type="button" className="button" onClick={() => clear(c)}>
              {t("Clear History")}
            </button>
          </Row>
        );
      })}
    </Group>
  );
}

// MARK: - Agents (UI/AgentSettings.swift)

/** A path as one shell word: quoted when it has spaces or quotes. */
function shellWord(path: string): string {
  return /^[\w@%+=:,./\\-]+$/.test(path) ? path : JSON.stringify(path);
}

const agentLogos = import.meta.glob("../assets/agent-logos/*.svg", { eager: true, query: "?url", import: "default" }) as Record<string, string>;
const agentApps: { id: AgentClientId; name: string; logo: string; mono?: boolean }[] = [
  { id: "claudeCode", name: "Claude Code", logo: "claudecode" },
  { id: "claudeDesktop", name: "Claude (chat)", logo: "claude" },
  { id: "codex", name: "ChatGPT (Codex)", logo: "chatgpt" },
  { id: "cursor", name: "Cursor", logo: "cursor", mono: true },
  { id: "windsurf", name: "Devin Desktop (Windsurf)", logo: "devin" },
  { id: "vsCode", name: "VS Code (Copilot)", logo: "vscode" },
  { id: "geminiCli", name: "Gemini CLI", logo: "geminicli" },
  { id: "grokCli", name: "Grok CLI", logo: "grok", mono: true },
  { id: "zed", name: "Zed", logo: "zed", mono: true },
];

function agentNote(id: AgentClientId, name: string): string {
  switch (id) {
    case "claudeCode": return t("The claude command and the Claude app's Code tab, every project.");
    case "claudeDesktop": return t("Chats in the Claude app. Restart it afterwards.");
    case "codex": return t("The app and the codex command (~/.codex/config.toml).");
    case "geminiCli":
    case "grokCli": return t("For every project.");
    default: return t("Restart %@ afterwards.", name);
  }
}

function agentMissing(id: AgentClientId, name: string): string {
  switch (id) {
    case "claudeCode": return t("Neither the Claude app nor the claude command was found.");
    case "geminiCli":
    case "grokCli": return t("Not installed (command not found).");
    default: return t("%@ isn't installed.", name);
  }
}

function AgentLogo({ logo, mono }: { logo: string; mono?: boolean }) {
  const url = agentLogos[`../assets/agent-logos/${logo}.svg`];
  return (
    <span className="agent-logo">
      {url && <img src={url} alt="" className={mono ? "mono-logo" : undefined} />}
    </span>
  );
}

function AgentsTab({ info, copy, say }: { info: SettingsInfo | null; copy: (text: string) => void; say: (text: string) => void }) {
  // Kept in the store, not settings.json: the MCP server is its own process and reads it there.
  const [manage, setManage] = useState<boolean | null>(null);
  const [connections, setConnections] = useState<SavedConnection[] | null>(null);
  const [statuses, setStatuses] = useState<AgentStatus[]>([]);
  const [tools, setTools] = useState<AgentTool[]>([]);
  const [working, setWorking] = useState<AgentClientId | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  useEffect(() => {
    void api.listConnections().then(
      (s) => {
        setManage(!!s.agentsManageConnections);
        setConnections([...s.connections].sort((a, b) => a.name.localeCompare(b.name)));
      },
      (e) => say(String(e)),
    );
    void api.agentClients().then(setStatuses, (e) => say(String(e)));
    void api.agentTools().then(setTools, (e) => say(String(e)));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  const changeManage = (on: boolean) => {
    setManage(on);
    void api.setAgentsManageConnections(on).catch((e) => {
      setManage(!on);
      say(String(e));
    });
  };
  const [choosing, setChoosing] = useState<string | null>(null);
  const changeAccess = (c: SavedConnection, access: AgentAccess) => {
    const put = (x: SavedConnection, a: AgentAccess) => ({ ...x, agentsHidden: a.hidden, agentsBlockedTools: a.blockedTools, agentsCanWrite: a.canWrite, agentsNotes: a.notes });
    const before = accessOf(c);
    setConnections((list) => list && list.map((x) => (x.id === c.id ? put(x, access) : x)));
    const saving = async () => {
      if (before.canWrite !== access.canWrite) await api.setAgentsCanWrite(c.id, access.canWrite);
      if (before.notes !== access.notes) await api.setAgentsNotes(c.id, access.notes);
      await api.setAgentsAccess(c.id, access.hidden, access.blockedTools);
    };
    void saving().catch((e) => {
      setConnections((list) => list && list.map((x) => (x.id === c.id ? put(x, before) : x)));
      say(String(e));
    });
  };
  const run = (app: { id: AgentClientId; name: string }, installed: boolean) => {
    setWorking(app.id);
    setProblem(null);
    api
      .setAgentInstalled(app.id, installed)
      .then(setStatuses, (e) => {
        setProblem(`${app.name}: ${String(e)}`);
        void api.agentClients().then(setStatuses);
      })
      .finally(() => setWorking(null));
  };
  const exe = info?.executable ?? "dblens";
  const command = `${shellWord(exe)} --mcp`;
  const json = JSON.stringify({ mcpServers: { dblens: { command: exe, args: ["--mcp"] } } }, null, 2);
  const action = (app: (typeof agentApps)[number], status: AgentStatus | undefined) => {
    if (!status) return null;
    if (working === app.id) return <span className="spinner small" />;
    if (status.installed && !status.current) {
      return (
        <span className="agent-actions">
          <span className="agent-state warn" title={status.installedCommand ?? ""}>{t("Old path")}</span>
          <button type="button" className="button" onClick={() => run(app, true)}>{t("Update")}</button>
        </span>
      );
    }
    if (status.installed) {
      return (
        <span className="agent-actions">
          <span className="agent-state ok">{t("Installed")}</span>
          <button type="button" className="button" onClick={() => run(app, false)}>{t("Remove")}</button>
        </span>
      );
    }
    return (
      <button type="button" className="button" disabled={!status.available} onClick={() => run(app, true)}>
        {t("Install")}
      </button>
    );
  };
  return (
    <>
      <p className="settings-intro">
        {t("Agents read your databases through the connections saved here. They see names, never URLs or passwords, and queries are read-only, enforced by the database.")}
      </p>
      <Group title={t("MCP Server")}>
        <Row label={t("Command")} detail={<code className="settings-code selectable">{command}</code>}>
          <button type="button" className="button" title={t("Copy the command that starts the MCP server")} onClick={() => copy(command)}>
            <Icon name="copy" size={14} />
            {t("Copy")}
          </button>
        </Row>
      </Group>
      <Group title={t("Install")} footer={problem ? <span className="settings-error selectable">{problem}</span> : undefined}>
        {agentApps.map((app) => {
          const status = statuses.find((s) => s.id === app.id);
          const name = app.id === "claudeDesktop" ? t(app.name) : app.name;
          const detail = status && (status.available || status.installed) ? agentNote(app.id, name) : agentMissing(app.id, name);
          return (
            <Row key={app.id} label={<span className="agent-label"><AgentLogo logo={app.logo} mono={app.mono} />{name}</span>} detail={detail}>
              {action(app, status)}
            </Row>
          );
        })}
        <Row label={<span className="agent-label"><span className="agent-logo other">…</span>{t("Other apps")}</span>} detail={t("Most take this in their MCP settings.")}>
          <button type="button" className="button" onClick={() => copy(json)}>
            <Icon name="copy" size={14} />
            {t("Copy JSON")}
          </button>
        </Row>
      </Group>
      <Group title={t("Tools")} footer={t("Tools that write work only on connections allowed below.")}>
        {tools.map((tool) => (
          <Row key={tool.name} label={<code className="settings-code">{tool.name}</code>} detail={tool.summary}>
            <span className={`tool-badge ${tool.writes ? "writes" : "reads"}`}>{tool.writes ? t("Writes") : t("Reads")}</span>
          </Row>
        ))}
      </Group>
      <Group title={t("Connections")}>
        <Toggle
          id="agents-manage"
          label={t("Let agents add and remove connections")}
          detail={t("create_connection and delete_connection. New connections start read-only.")}
          checked={manage ?? false}
          disabled={manage === null}
          onChange={changeManage}
        />
      </Group>
      <Group
        title={t("Access by Connection")}
        footer={t("Hidden connections don't show up for agents. Tools that write also need Let Agents Write. Everything agents run shows in the SQL panel's History.")}
      >
        {connections && connections.length === 0 && <p className="settings-intro">{t("No saved connections yet.")}</p>}
        {(connections ?? []).map((c) => (
          <Fragment key={c.id}>
            <Row label={c.name} detail={accessSummary(c)}>
              <button
                type="button"
                className="button"
                aria-expanded={choosing === c.id}
                title={t("Choose which tools agents may use on this connection")}
                onClick={() => setChoosing(choosing === c.id ? null : c.id)}
              >
                {choosing === c.id ? t("Done") : t("Choose…")}
              </button>
            </Row>
            {choosing === c.id && (
              <div className="agent-access">
                <AgentAccessEditor connection={c} onChange={(access) => changeAccess(c, access)} />
              </div>
            )}
          </Fragment>
        ))}
      </Group>
    </>
  );
}

// MARK: - Updates (UpdateSettings in SettingsView.swift)

function UpdatesTab({ settings, change, info, say }: { settings: Settings; change: Change; info: SettingsInfo | null; say: (text: string) => void }) {
  const [checking, setChecking] = useState(false);
  const [lastCheck, setLastCheck] = useState<number | null>(null);
  useEffect(() => {
    if (!checking) void api.lastUpdateCheck().then(setLastCheck, () => setLastCheck(null));
  }, [checking]);
  const check = () => {
    setChecking(true);
    // A found update also reaches the main window, whose notice offers to install it.
    void api
      .checkForUpdates()
      .then(
        (found) => say(found.available && found.version ? t("dbLens %@ is available", found.version) : t("dbLens is up to date.")),
        (e) => say(String(e)),
      )
      .finally(() => setChecking(false));
  };
  return (
    <Group footer={t("Updates come from GitHub Releases and install only when their signature matches dbLens's key.")}>
      <Row label={t("Version")}>
        <span className="selectable secondary">{info?.version ?? "–"}</span>
      </Row>
      <Toggle
        id="check-updates"
        label={t("Check for updates automatically")}
        checked={settings.checkForUpdates}
        onChange={(on) => change({ checkForUpdates: on })}
      />
      <Toggle
        id="auto-update"
        label={t("Update automatically")}
        detail={t("Downloads in the background and installs when you quit; a notice in the window lets you restart now.")}
        checked={settings.updateAutomatically}
        disabled={!settings.checkForUpdates}
        onChange={(on) => change({ updateAutomatically: on })}
      />
      <Row label={t("Last checked")} detail={lastCheck ? relativeTime(lastCheck * 1000) : t("Never")}>
        <button type="button" className="button" disabled={checking} onClick={check}>
          {t("Check Now")}
        </button>
      </Row>
    </Group>
  );
}

/** "5 minutes ago", "yesterday", in the UI language. */
function relativeTime(at: number): string {
  const seconds = Math.round((at - Date.now()) / 1000);
  const format = new Intl.RelativeTimeFormat(language, { numeric: "auto" });
  const steps: [Intl.RelativeTimeFormatUnit, number][] = [["second", 60], ["minute", 60], ["hour", 24], ["day", 30], ["month", 12]];
  let value = seconds;
  for (const [unit, size] of steps) {
    if (Math.abs(value) < size) return format.format(value, unit);
    value = Math.round(value / size);
  }
  return format.format(value, "year");
}

// MARK: - Data & Privacy (DataSettings in SettingsView.swift)

function DataTab({ info, say }: { info: SettingsInfo | null; say: (text: string) => void }) {
  const [confirming, setConfirming] = useState(false);
  const folder = info?.dataFolder ?? null;
  const fileManager = info?.os === "macos" ? t("Show in Finder") : info?.os === "windows" ? t("Show in Explorer") : t("Show Folder");
  const clearAll = () => {
    setConfirming(false);
    void api.clearSqlHistory().then(() => say(t("Cleared all history")), (e) => say(String(e)));
  };
  return (
    <>
      <Group
        title={t("Stored on this computer")}
        footer={t("Connection URLs are encrypted with AES-GCM; the key is in the system's keychain (or a file only your user can read). On another computer, paste the URLs again.")}
      >
        <Row
          label={t("Data folder")}
          detail={<span className="selectable settings-path">{folder ?? t("Nothing is kept on disk in this session.")}</span>}
        >
          <button type="button" className="button" disabled={!folder} onClick={() => void showDataFolder().catch((e) => say(String(e)))}>
            {fileManager}
          </button>
        </Row>
        <Row label={t("Connection names and SSH settings")} detail={<code className="settings-code">connections.json</code>} />
        <Row label={t("Connection URLs, encrypted")} detail={<code className="settings-code">connections.enc</code>} />
        <Row label={t("Settings")} detail={<code className="settings-code">settings.json</code>} />
      </Group>
      <Group>
        <Row label={t("SQL history")} detail={t("What you ran, per saved connection (up to 200 each).")}>
          {!confirming && (
            <button type="button" className="button destructive" onClick={() => setConfirming(true)}>
              {t("Clear All History…")}
            </button>
          )}
        </Row>
        {confirming && (
          <div className="form-row settings-confirm" role="alertdialog" aria-label={t("Clear the SQL history of every connection?")}>
            <span className="settings-row-label">{t("Clear the SQL history of every connection?")}</span>
            <div className="settings-row-control">
              <button type="button" className="button" onClick={() => setConfirming(false)}>
                {t("Cancel")}
              </button>
              <button type="button" className="button prominent danger" onClick={clearAll}>
                {t("Clear All History")}
              </button>
            </div>
          </div>
        )}
      </Group>
    </>
  );
}
