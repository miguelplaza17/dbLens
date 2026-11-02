// The Settings window (Sources/dbLens/SettingsView.swift): a sidebar of topics
// and one grouped form per topic. It runs in its own Tauri window (label
// "settings", opened by Settings… / Mod+,) and sizes itself to its content.
// Settings live in Rust (src-tauri/src/settings.rs); every change reaches all
// windows at once.

import { useCallback, useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { api, engineNames, inTauri, type SavedConnection, type StoreSnapshot } from "../api";
import { copyText } from "../format";
import { t } from "../i18n";
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

export type SettingsTab = "appearance" | "connections" | "agents" | "updates" | "data";

const TABS: { id: SettingsTab; title: string; icon: IconName }[] = [
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
  return TABS.some((tab) => tab.id === id) ? (id as SettingsTab) : "appearance";
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

// MARK: - Appearance (AppearanceSettings.swift, Theme.swift)

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

function AgentsTab({ info, copy, say }: { info: SettingsInfo | null; copy: (text: string) => void; say: (text: string) => void }) {
  // Kept in the store, not settings.json: the MCP server is its own process and reads it there.
  const [manage, setManage] = useState<boolean | null>(null);
  useEffect(() => {
    void api.listConnections().then((s) => setManage(!!s.agentsManageConnections), (e) => say(String(e)));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  const changeManage = (on: boolean) => {
    setManage(on);
    void api.setAgentsManageConnections(on).catch((e) => {
      setManage(!on);
      say(String(e));
    });
  };
  const exe = info?.executable ?? "dblens";
  const command = `${shellWord(exe)} --mcp`;
  const claude = `claude mcp add --scope user dblens -- ${command}`;
  const codex = `codex mcp add dblens -- ${command}`;
  const json = JSON.stringify({ mcpServers: { dblens: { command: exe, args: ["--mcp"] } } }, null, 2);
  const commandRow = (label: string, text: string, title: string) => (
    <Row label={label} detail={<code className="settings-code selectable">{text}</code>}>
      <button type="button" className="button" title={title} onClick={() => copy(text)}>
        <Icon name="copy" size={14} />
        {t("Copy")}
      </button>
    </Row>
  );
  return (
    <>
      <p className="settings-intro">
        {t("Agents read your databases through the connections saved here. They see names, never URLs or passwords, and queries are read-only, enforced by the database.")}
      </p>
      <Group title={t("MCP Server")}>
        {commandRow(t("Command"), command, t("Copy the command that starts the MCP server"))}
      </Group>
      <Group title={t("Install")} footer={t("Restart the app afterwards, or start a new session.")}>
        {commandRow("Claude Code", claude, t("Copy the command that adds dbLens to Claude Code"))}
        {commandRow("Codex", codex, t("Copy the command that adds dbLens to Codex"))}
        <Row label={t("Other apps")} detail={t("Most take this in their MCP settings.")}>
          <button type="button" className="button" onClick={() => copy(json)}>
            <Icon name="copy" size={14} />
            {t("Copy JSON")}
          </button>
        </Row>
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
    </>
  );
}

// MARK: - Updates (UpdateSettings in SettingsView.swift)

function UpdatesTab({ settings, change, info, say }: { settings: Settings; change: Change; info: SettingsInfo | null; say: (text: string) => void }) {
  const [checking, setChecking] = useState(false);
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
      <Row label={t("Check for Updates")}>
        <button type="button" className="button" disabled={checking} onClick={check}>
          {t("Check Now")}
        </button>
      </Row>
    </Group>
  );
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
