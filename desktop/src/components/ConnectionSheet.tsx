// New or edit connection, two ways: paste a URL (the engine is read from it as
// you type), or fill in host, port, database, user, and password. Connect
// proves it works before it is saved (ConnectionSheet.swift).

import { useEffect, useMemo, useRef, useState } from "react";
import { api, engineNames, errorText, type Engine, type SavedConnection, type SshConfig, type Summary } from "../api";
import { t } from "../i18n";
import { useModel } from "../model";
import { compose, decompose, defaultPorts, engines, sslModes, type UrlFields } from "../urlForm";
import { Icon } from "./Icon";
import { Sheet } from "./Sheet";

type Mode = "url" | "fields";

export function ConnectionSheet({ editing, initialUrl }: { editing: SavedConnection | null; initialUrl?: string }) {
  const model = useModel();
  const [mode, setMode] = useState<Mode>("url");
  const [url, setUrl] = useState(initialUrl ?? "");
  const [name, setName] = useState(editing?.name ?? "");
  const [fields, setFields] = useState<UrlFields>({ engine: "postgres", host: "", port: "", database: "", user: "", password: "", sslmode: "" });
  const [useSSH, setUseSSH] = useState(!!editing?.ssh);
  const [ssh, setSsh] = useState({
    host: editing?.ssh?.host ?? "",
    port: editing?.ssh?.port ? String(editing.ssh.port) : "",
    user: editing?.ssh?.user ?? "",
    keyPath: editing?.ssh?.keyPath ?? "",
  });
  const [detected, setDetected] = useState<Engine | null>(null);
  const [summary, setSummary] = useState<Summary | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [working, setWorking] = useState(false);
  const urlField = useRef<HTMLInputElement>(null);

  // Editing: the saved URL comes back with its password masked.
  useEffect(() => {
    if (!editing) return;
    api.connectionUrl(editing.id).then(setUrl, (e) => setError(errorText(e)));
  }, [editing]);

  useEffect(() => {
    if (!editing) urlField.current?.focus();
  }, [editing]);

  const text = mode === "fields" ? compose(fields) : url;
  const engine: Engine | null = mode === "url" ? detected : fields.engine;

  // The engine and the parts, read live by the backend as the text changes.
  useEffect(() => {
    let current = true;
    const handle = setTimeout(async () => {
      const [found, parts] = await Promise.all([
        api.detectEngine(text).catch(() => null),
        api.summarizeUrl(text).catch(() => null),
      ]);
      if (!current) return;
      setDetected(found);
      setSummary(parts);
    }, 80);
    return () => {
      current = false;
      clearTimeout(handle);
    };
  }, [text]);

  const sshConfig: SshConfig | null = useMemo(() => {
    if (!useSSH || !ssh.host.trim()) return null;
    const port = Number(ssh.port.trim());
    return { host: ssh.host.trim(), port: port > 0 ? port : null, user: ssh.user.trim(), keyPath: ssh.keyPath.trim() };
  }, [useSSH, ssh]);

  const canConnect =
    (mode === "url" ? !!url.trim() : !!(fields.engine === "sqlite" ? fields.database : fields.host).trim()) &&
    !!engine &&
    (!useSSH || !!ssh.host.trim()) &&
    !working;

  const switchMode = (next: Mode) => {
    if (next === mode) return;
    setError(null);
    if (next === "fields") {
      setFields(decompose(url, detected ?? fields.engine));
    } else if ((fields.engine === "sqlite" ? fields.database : fields.host).trim()) {
      setUrl(compose(fields));
    }
    setMode(next);
  };

  const save = async () => {
    if (!canConnect) return;
    setWorking(true);
    setError(null);
    try {
      await model.connectNew({ url: text, name: name.trim(), ssh: engine === "sqlite" ? null : sshConfig, editing });
      model.open(null);
    } catch (e) {
      setError(errorText(e));
    }
    setWorking(false);
  };

  const footer = (() => {
    if (mode === "url" && !engine && url.trim()) return t("This doesn't look like a database URL.");
    if (mode === "fields") return t("Stored only for your user account on this computer.");
    if (summary) {
      const parts = [t("database %@", summary.database)];
      if (summary.host) parts.push(t("host %@", summary.host));
      if (summary.port) parts.push(t("port %@", String(summary.port)));
      if (summary.user) parts.push(t("user %@", summary.user));
      if (summary.tls) parts.push(t("SSL %@", summary.tls));
      return t("Understood: %@", parts.join(" · "));
    }
    return t(
      "Paste any connection URL: the kind of database is read from it. .env lines (DATABASE_URL=…) work too. Stored only for your user account on this computer.",
    );
  })();

  const field = (key: keyof UrlFields, label: string, placeholder: string, type = "text") => (
    <label className="form-row">
      <span className="form-label">{t(label)}</span>
      <input
        type={type}
        value={fields[key]}
        placeholder={placeholder}
        spellCheck={false}
        autoComplete="off"
        onChange={(e) => setFields({ ...fields, [key]: e.target.value })}
      />
    </label>
  );

  return (
    <Sheet label={editing ? t("Edit Connection") : t("New Connection")} onClose={() => !working && model.open(null)}>
      <form
        className="sheet-body"
        onSubmit={(e) => {
          e.preventDefault();
          void save();
        }}
      >
        <div className="segmented" role="tablist">
          {(["url", "fields"] as Mode[]).map((m) => (
            <button key={m} type="button" role="tab" aria-selected={mode === m} className={mode === m ? "selected" : ""} onClick={() => switchMode(m)}>
              {m === "url" ? "URL" : t("Fields")}
            </button>
          ))}
        </div>

        <section className="form-group">
          <header className="form-group-header">
            <span>{editing ? t("Edit Connection") : t("New Connection")}</span>
            {mode === "url" && engine && <span className="engine-tag">{engineNames[engine]}</span>}
          </header>
          <div className="form-card">
            {mode === "url" ? (
              <label className="form-row">
                <span className="form-label">URL</span>
                <input
                  ref={urlField}
                  className="mono"
                  value={url}
                  placeholder="postgres://user:password@host:5432/database"
                  spellCheck={false}
                  autoComplete="off"
                  onChange={(e) => setUrl(e.target.value)}
                />
              </label>
            ) : (
              <>
                <label className="form-row">
                  <span className="form-label">{t("Type")}</span>
                  <select value={fields.engine} onChange={(e) => setFields({ ...fields, engine: e.target.value as Engine })}>
                    {engines.map((e) => (
                      <option key={e} value={e}>
                        {engineNames[e]}
                      </option>
                    ))}
                  </select>
                </label>
                {fields.engine === "sqlite" ? (
                  field("database", "File", "~/projects/app/data.sqlite")
                ) : (
                  <>
                    {field("host", "Host", "db.example.com")}
                    {field("port", "Port", String(defaultPorts[fields.engine] ?? ""))}
                    {field("database", "Database", fields.user || "postgres")}
                    {field("user", "User", "postgres")}
                    {field("password", "Password", "", "password")}
                    {fields.engine === "postgres" && (
                      <label className="form-row">
                        <span className="form-label">SSL</span>
                        <select value={fields.sslmode} onChange={(e) => setFields({ ...fields, sslmode: e.target.value })}>
                          {sslModes.map(([value, title]) => (
                            <option key={value} value={value}>
                              {t(title)}
                            </option>
                          ))}
                        </select>
                      </label>
                    )}
                  </>
                )}
              </>
            )}
            <label className="form-row">
              <span className="form-label">{t("Name")}</span>
              <input value={name} placeholder={summary?.suggestedName ?? t("Optional")} spellCheck={false} onChange={(e) => setName(e.target.value)} />
            </label>
          </div>
          <p className="form-footer">{footer}</p>
        </section>

        {engine !== "sqlite" && (
          <section className="form-group">
            <div className="form-card">
              <label className="form-row toggle-row">
                <span className="form-label wide">{t("Connect via SSH")}</span>
                <input type="checkbox" className="switch" checked={useSSH} onChange={(e) => setUseSSH(e.target.checked)} />
              </label>
              {useSSH && (
                <>
                  <SshField label="Server" value={ssh.host} placeholder={t("host, IP, or user@host")} onChange={(host) => setSsh({ ...ssh, host })} />
                  <SshField label="Port" value={ssh.port} placeholder="22" onChange={(port) => setSsh({ ...ssh, port })} />
                  <SshField label="User" value={ssh.user} placeholder={t("from ~/.ssh/config")} onChange={(user) => setSsh({ ...ssh, user })} />
                  <SshField label="Key" value={ssh.keyPath} placeholder={t("ssh-agent or ~/.ssh/config")} onChange={(keyPath) => setSsh({ ...ssh, keyPath })} />
                </>
              )}
            </div>
            {useSSH && (
              <p className="form-footer">
                {t(
                  "Uses this computer's ssh: your keys, ssh-agent, and ~/.ssh/config. The database host is resolved by the server, so use localhost for a database on the server itself.",
                )}
              </p>
            )}
          </section>
        )}

        {error && (
          <div className="form-error" role="alert">
            <Icon name="alert" size={14} />
            <span className="selectable">{error}</span>
          </div>
        )}

        <footer className="sheet-actions">
          {working && (
            <>
              <span className="spinner small" />
              <span className="secondary">{t("Connecting…")}</span>
            </>
          )}
          <span className="spacer" />
          <button type="button" className="button" onClick={() => model.open(null)} disabled={working}>
            {t("Cancel")}
          </button>
          <button type="submit" className="button prominent" disabled={!canConnect}>
            {editing ? t("Save and Connect") : t("Connect")}
          </button>
        </footer>
      </form>
    </Sheet>
  );
}

function SshField({ label, value, placeholder, onChange }: { label: string; value: string; placeholder: string; onChange: (v: string) => void }) {
  return (
    <label className="form-row">
      <span className="form-label">{t(label)}</span>
      <input value={value} placeholder={placeholder} spellCheck={false} autoComplete="off" onChange={(e) => onChange(e.target.value)} />
    </label>
  );
}
