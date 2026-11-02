//! Open in Terminal: the database's own command-line client (psql, mysql,
//! sqlite3, redis-cli, mongosh) for a saved connection, started in the
//! platform's terminal. Port of `Sources/DBLensKit/ClientCommand.swift` and
//! `openInTerminal` in `Models.swift`.
//!
//! The password goes in an environment variable the client reads, never in the
//! arguments (which any process of the user can list). The variables are set
//! by a small private script that deletes itself first, so the password lives
//! on disk only until the client starts. mongosh reads no password from the
//! environment, so its URL is left without one and mongosh asks for it.
//!
//! Also here: the saved URL with its password, for "Copy URL with Password".

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::State;

use crate::commands::AppState;
use crate::drivers::mysql::ssl_mode;
use crate::drivers::redis::RedisUrl;
use crate::engine::Engine;
use crate::menu::Os;
use crate::session::{DbError, DbResult};
use crate::url::{encode, ConnectionUrl, SqliteLocation, UrlParts};

/// What to run for a connection, before it is tied to an installed tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientCommand {
    pub engine: Engine,
    /// Executable names to look for, in order of preference.
    pub tools: Vec<&'static str>,
    pub arguments: Vec<String>,
    pub environment: BTreeMap<String, String>,
}

impl ClientCommand {
    /// The command for a saved connection's text. `tunnel_port` is the local
    /// end of the app's SSH tunnel, when the connection goes through one.
    pub fn make(text: &str, tunnel_port: Option<u16>) -> DbResult<ClientCommand> {
        let engine = Engine::detect(text)
            .ok_or_else(|| DbError::msg("This doesn't look like a database URL."))?;
        let mut environment = BTreeMap::new();
        let (tools, arguments) = match engine {
            Engine::Postgres => {
                let url = ConnectionUrl::new(text)?;
                if let Some(password) = url.password() {
                    environment.insert("PGPASSWORD".to_string(), password.to_string());
                }
                (
                    vec!["psql"],
                    vec![url.conninfo_without_password(tunnel_port)],
                )
            }
            Engine::Mysql => {
                let parts = UrlParts::new(text)
                    .ok_or_else(|| DbError::msg("This doesn't look like a database URL."))?;
                let socket = parts
                    .query
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("socket"))
                    .map(|(_, v)| v.clone())
                    .filter(|s| !s.is_empty());
                let mut args = match (&socket, tunnel_port) {
                    (Some(socket), None) => vec![format!("--socket={socket}")],
                    _ => {
                        let host = match tunnel_port {
                            Some(_) => "127.0.0.1".to_string(),
                            None => parts
                                .first_host()
                                .map(|h| h.name.clone())
                                .filter(|h| !h.is_empty())
                                .unwrap_or_else(|| "localhost".into()),
                        };
                        let port = tunnel_port
                            .or_else(|| parts.first_host().and_then(|h| h.port))
                            .unwrap_or(3306);
                        vec![
                            format!("--host={host}"),
                            format!("--port={port}"),
                            "--protocol=TCP".into(),
                        ]
                    }
                };
                if let Some(user) = parts.user.as_ref().filter(|u| !u.is_empty()) {
                    args.push(format!("--user={user}"));
                }
                if let Some(mode) = ssl_mode(&parts.query) {
                    args.push(format!("--ssl-mode={mode}"));
                }
                if let Some(database) = parts.path.split('/').next().filter(|d| !d.is_empty()) {
                    // A name like "--execute=…" would be read as an option.
                    if database.starts_with('-') {
                        return Err(DbError::msg("The database name can't start with a dash."));
                    }
                    args.push(database.to_string());
                }
                if let Some(password) = parts.password.as_ref().filter(|p| !p.is_empty()) {
                    environment.insert("MYSQL_PWD".to_string(), password.clone());
                }
                (vec!["mysql", "mariadb"], args)
            }
            Engine::Sqlite => {
                let mut path = SqliteLocation::new(text)?.path;
                if path.starts_with('-') {
                    // Keep sqlite3 from reading the file name as an option.
                    path.insert_str(0, "./");
                }
                (vec!["sqlite3"], vec![path])
            }
            Engine::Redis => {
                let url = RedisUrl::parse(text)?;
                let mut args = vec![
                    "-h".to_string(),
                    if tunnel_port.is_some() {
                        "127.0.0.1".into()
                    } else {
                        url.host.clone()
                    },
                    "-p".into(),
                    tunnel_port.unwrap_or(url.port).to_string(),
                    "-n".into(),
                    url.database.to_string(),
                ];
                if let Some(user) = &url.user {
                    args.extend(["--user".to_string(), user.clone()]);
                }
                if url.tls {
                    args.push("--tls".into());
                    if !url.verify_tls {
                        args.push("--insecure".into());
                    }
                }
                if let Some(password) = &url.password {
                    environment.insert("REDISCLI_AUTH".to_string(), password.clone());
                }
                (vec!["redis-cli", "valkey-cli"], args)
            }
            Engine::Mongodb => (vec!["mongosh"], vec![mongo_target(text, tunnel_port)?]),
        };
        Ok(ClientCommand {
            engine,
            tools,
            arguments,
            environment,
        })
    }

    /// How to get the first tool, in words for this OS.
    pub fn install_hint(&self, os: Os) -> &'static str {
        match (os, self.engine) {
            (Os::Mac, Engine::Postgres) => "brew install libpq",
            (Os::Mac, Engine::Mysql) => "brew install mysql-client",
            (Os::Mac, Engine::Sqlite) => "brew install sqlite",
            (Os::Mac, Engine::Redis) => "brew install redis",
            (Os::Mac, Engine::Mongodb) => "brew install mongosh",
            (Os::Linux, Engine::Postgres) => "sudo apt install postgresql-client",
            (Os::Linux, Engine::Mysql) => "sudo apt install mysql-client",
            (Os::Linux, Engine::Sqlite) => "sudo apt install sqlite3",
            (Os::Linux, Engine::Redis) => "sudo apt install redis-tools",
            (Os::Linux, Engine::Mongodb) => "https://www.mongodb.com/try/download/shell",
            (Os::Windows, Engine::Postgres) => "winget install PostgreSQL.PostgreSQL.17",
            (Os::Windows, Engine::Mysql) => "winget install Oracle.MySQL",
            (Os::Windows, Engine::Sqlite) => "https://sqlite.org/download.html (sqlite-tools)",
            (Os::Windows, Engine::Redis) => "https://github.com/redis-windows/redis-windows",
            (Os::Windows, Engine::Mongodb) => "winget install MongoDB.Shell",
        }
    }
}

/// The URL for mongosh, without its password (mongosh asks for it). Through
/// an SSH tunnel it points at the local end, like the app's own connection.
fn mongo_target(text: &str, tunnel_port: Option<u16>) -> DbResult<String> {
    let parts = UrlParts::new(text)
        .filter(|p| p.scheme.starts_with("mongodb"))
        .ok_or_else(|| DbError::msg("This doesn't look like a database URL."))?;
    // A password with an unencoded "/" splits the URL in the wrong place and
    // leaves a "host" that is not one; refuse rather than guess.
    let bad_host = |name: &str| {
        if name.contains(':') {
            !name.chars().all(|c| c.is_ascii_hexdigit() || c == ':')
        } else {
            name.chars()
                .any(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')))
        }
    };
    if parts.hosts.iter().any(|h| bad_host(&h.name)) {
        return Err(DbError::msg(
            "The URL has a character in the host that needs percent-encoding (a password with / or @?).",
        ));
    }
    let auth = parts
        .user
        .as_ref()
        .filter(|u| !u.is_empty())
        .map(|u| format!("{}@", encode(u)))
        .unwrap_or_default();
    let mut query: Vec<String> = parts
        .query
        .iter()
        .filter(|(k, v)| !is_secret_option(k, v))
        .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
        .collect();
    query.sort();
    let (scheme, hosts) = match tunnel_port {
        Some(port) => {
            query.push("directConnection=true".into());
            ("mongodb", format!("127.0.0.1:{port}"))
        }
        None => {
            let hosts: Vec<String> = parts
                .hosts
                .iter()
                .map(|h| {
                    let name = if h.name.contains(':') {
                        format!("[{}]", h.name)
                    } else {
                        h.name.clone()
                    };
                    match h.port {
                        Some(port) => format!("{name}:{port}"),
                        None => name,
                    }
                })
                .collect();
            let scheme = if parts.scheme == "mongodb+srv" {
                "mongodb+srv"
            } else {
                "mongodb"
            };
            (scheme, hosts.join(","))
        }
    };
    let query = if query.is_empty() {
        String::new()
    } else {
        format!("?{}", query.join("&"))
    };
    Ok(format!(
        "{scheme}://{auth}{hosts}/{}{query}",
        encode(&parts.path)
    ))
}

/// A URL option that carries a secret, which must not reach the arguments.
fn is_secret_option(key: &str, value: &str) -> bool {
    let words = ["password", "passwd", "pwd", "secret", "token"];
    let key = key.to_ascii_lowercase();
    if words.iter().any(|w| key.contains(w)) {
        return true;
    }
    key == "authmechanismproperties" && {
        let value = value.to_ascii_lowercase();
        words.iter().any(|w| value.contains(w))
    }
}

/// `scheme://user:password@host/…` without `:password`.
pub fn strip_password(url: &str) -> String {
    let Some(start) = url.find("://").map(|i| i + 3) else {
        return url.to_string();
    };
    let rest = &url[start..];
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let Some(at) = rest[..end].rfind('@') else {
        return url.to_string();
    };
    let userinfo = &rest[..at];
    match userinfo.find(':') {
        Some(colon) => format!("{}{}{}", &url[..start], &userinfo[..colon], &rest[at..]),
        None => url.to_string(),
    }
}

// MARK: - Quoting and scripts

/// For sh: 'it'\''s'.
pub fn posix_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// For PowerShell: 'it''s'. PowerShell also reads the typographic single
/// quotes as quotes, so they are doubled too.
pub fn powershell_quote(value: &str) -> String {
    let mut out = String::from("'");
    for c in value.chars() {
        out.push(c);
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}') {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// For a cmd.exe command line: "C:\Program Files\x". None when the text has
/// something no quoting makes safe there (a quote, %, or a line break).
pub fn cmd_quote(value: &str) -> Option<String> {
    if value.contains(['"', '%', '\r', '\n', '\0']) {
        return None;
    }
    Some(format!("\"{value}\""))
}

/// A string for an AppleScript `do script "…"`.
fn applescript_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// An sh script that removes itself, sets the environment, and becomes the client.
pub fn posix_script(executable: &str, command: &ClientCommand) -> String {
    let mut lines = vec!["#!/bin/sh".to_string(), "rm -f -- \"$0\"".to_string()];
    for (key, value) in &command.environment {
        lines.push(format!("export {key}={}", posix_quote(value)));
    }
    lines.push("clear".into());
    let mut run = vec!["exec".to_string(), posix_quote(executable)];
    run.extend(command.arguments.iter().map(|a| posix_quote(a)));
    lines.push(run.join(" "));
    lines.join("\n") + "\n"
}

/// A PowerShell script that removes itself, sets the environment, and runs the client.
pub fn powershell_script(executable: &str, command: &ClientCommand) -> String {
    let mut lines = vec![
        "Remove-Item -LiteralPath $PSCommandPath -Force -ErrorAction SilentlyContinue".to_string(),
    ];
    for (key, value) in &command.environment {
        lines.push(format!("$env:{key} = {}", powershell_quote(value)));
    }
    lines.push("Clear-Host".into());
    let mut run = vec!["&".to_string(), powershell_quote(executable)];
    run.extend(command.arguments.iter().map(|a| powershell_quote(a)));
    lines.push(run.join(" "));
    // Without a BOM, Windows PowerShell 5 reads a script as ANSI and mangles names.
    format!("\u{feff}{}\r\n", lines.join("\r\n"))
}

// MARK: - Finding the tool and the terminal

fn exe_names(tool: &str, os: Os) -> Vec<String> {
    match os {
        Os::Windows => vec![format!("{tool}.exe"), format!("{tool}.cmd")],
        _ => vec![tool.to_string()],
    }
}

/// The first installed tool in `folders`, trying the tools in order.
pub fn resolve(tools: &[&str], folders: &[PathBuf], os: Os) -> Option<PathBuf> {
    tools.iter().find_map(|tool| {
        folders.iter().find_map(|folder| {
            exe_names(tool, os)
                .into_iter()
                .map(|name| folder.join(name))
                .find(|path| path.is_file())
        })
    })
}

/// Subfolders of `root` that have a `bin` (versioned installs: PostgreSQL/16/bin).
fn versioned_bins(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return vec![];
    };
    let mut bins: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path().join("bin"))
        .filter(|p| p.is_dir())
        .collect();
    bins.sort();
    bins.reverse();
    bins
}

/// PATH, then the places these clients install to. A GUI app does not always
/// get the shell's PATH (macOS), and Windows installers do not add theirs.
fn search_folders(os: Os) -> Vec<PathBuf> {
    let mut folders: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    match os {
        Os::Mac => {
            for dir in [
                "/opt/homebrew/bin",
                "/usr/local/bin",
                "/opt/homebrew/opt/libpq/bin",
                "/opt/homebrew/opt/mysql-client/bin",
                "/opt/homebrew/opt/sqlite/bin",
                "/usr/local/opt/libpq/bin",
                "/Applications/Postgres.app/Contents/Versions/latest/bin",
                "/usr/bin",
            ] {
                folders.push(dir.into());
            }
            for v in versioned_bins(Path::new("/opt/homebrew/opt")) {
                if v.to_string_lossy().contains("postgresql@") {
                    folders.push(v);
                }
            }
        }
        Os::Linux => {
            folders.extend(versioned_bins(Path::new("/usr/lib/postgresql")));
            for dir in ["/usr/local/bin", "/usr/bin", "/snap/bin"] {
                folders.push(dir.into());
            }
        }
        Os::Windows => {
            for var in ["ProgramFiles", "ProgramFiles(x86)"] {
                if let Some(root) = std::env::var_os(var).map(PathBuf::from) {
                    for product in ["PostgreSQL", "MySQL", "MariaDB", "MongoDB/Server"] {
                        let dir = root.join(product);
                        folders.extend(versioned_bins(&dir));
                        // MySQL Server 8.4/bin, MariaDB 11.4/bin
                        if let Ok(entries) = std::fs::read_dir(&root) {
                            for e in entries.flatten() {
                                let name = e.file_name().to_string_lossy().into_owned();
                                if name.starts_with(product.split('/').next().unwrap_or(product)) {
                                    folders.push(e.path().join("bin"));
                                    folders.extend(versioned_bins(&e.path()));
                                }
                            }
                        }
                    }
                    folders.push(root.join("SQLite"));
                    folders.push(root.join("Redis"));
                }
            }
            if let Some(local) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
                folders.push(local.join("Programs").join("mongosh"));
            }
        }
    }
    folders
}

/// How to start a terminal that runs `script`: the program and its arguments.
/// With `raw`, the arguments are one command line to hand over as it is
/// (Windows `cmd /c start`, which has its own quoting rules).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    pub program: String,
    pub args: Vec<String>,
    pub raw: bool,
}

fn launch(program: &str, args: &[&str]) -> Launch {
    Launch {
        program: program.into(),
        args: args.iter().map(|a| a.to_string()).collect(),
        raw: false,
    }
}

const POWERSHELL: [&str; 6] = [
    "powershell.exe",
    "-NoLogo",
    "-NoProfile",
    "-ExecutionPolicy",
    "Bypass",
    "-File",
];

/// The terminal to use for `script`, from what `available` says is installed.
/// `preferred` is the user's own choice ($TERMINAL), tried first on Linux.
pub fn terminal_launch(
    os: Os,
    script: &Path,
    preferred: Option<&str>,
    available: &dyn Fn(&str) -> bool,
) -> Option<Launch> {
    let path = script.to_string_lossy().into_owned();
    match os {
        Os::Mac => {
            let line = applescript_string(&format!("sh {}", posix_quote(&path)));
            Some(Launch {
                program: "osascript".into(),
                args: vec![
                    "-e".into(),
                    format!("tell application \"Terminal\" to do script \"{line}\""),
                    "-e".into(),
                    "tell application \"Terminal\" to activate".into(),
                ],
                raw: false,
            })
        }
        Os::Windows => {
            if available("wt.exe") {
                let mut args: Vec<&str> = POWERSHELL.to_vec();
                args.push(&path);
                return Some(launch("wt.exe", &args));
            }
            // The classic console: `start` opens a new window for the command.
            let quoted = cmd_quote(&path)?;
            let mut line = vec!["/c".to_string(), "start".into(), "\"dbLens\"".into()];
            line.extend(POWERSHELL.iter().map(|a| a.to_string()));
            line.push(quoted);
            Some(Launch {
                program: "cmd.exe".into(),
                args: line,
                raw: true,
            })
        }
        Os::Linux => {
            let shell = |flag: &[&'static str]| -> Vec<String> {
                flag.iter()
                    .map(|a| a.to_string())
                    .chain(["sh".to_string(), path.clone()])
                    .collect()
            };
            let mut candidates: Vec<(String, Vec<String>)> = vec![];
            if let Some(own) =
                preferred.filter(|p| !p.is_empty() && !p.contains(char::is_whitespace))
            {
                candidates.push((own.to_string(), shell(&["-e"])));
            }
            for (name, flag) in [
                ("x-terminal-emulator", &["-e"][..]),
                ("gnome-terminal", &["--"][..]),
                ("konsole", &["-e"][..]),
                ("xfce4-terminal", &["-x"][..]),
                ("mate-terminal", &["-x"][..]),
                ("kitty", &[][..]),
                ("alacritty", &["-e"][..]),
                ("foot", &[][..]),
                ("xterm", &["-e"][..]),
            ] {
                candidates.push((name.to_string(), shell(flag)));
            }
            candidates
                .into_iter()
                .find(|(name, _)| available(name))
                .map(|(program, args)| Launch {
                    program,
                    args,
                    raw: false,
                })
        }
    }
}

fn on_path(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|dir| dir.join(name).exists()))
        .unwrap_or(false)
        || (cfg!(windows) && windows_apps_alias(name))
}

/// `wt.exe` lives in the per-user WindowsApps folder.
fn windows_apps_alias(name: &str) -> bool {
    std::env::var_os("LOCALAPPDATA")
        .map(|d| {
            PathBuf::from(d)
                .join("Microsoft/WindowsApps")
                .join(name)
                .exists()
        })
        .unwrap_or(false)
}

/// Writes the script where only this user can read it.
fn write_script(text: &str, os: Os) -> DbResult<PathBuf> {
    use std::io::Write;
    let extension = if os == Os::Windows { "ps1" } else { "sh" };
    let path = std::env::temp_dir().join(format!("dblens-{}.{extension}", uuid::Uuid::new_v4()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o700);
    }
    options
        .open(&path)
        .and_then(|mut f| f.write_all(text.as_bytes()))
        .map_err(|e| DbError::Message(format!("Open in Terminal: {e}")))?;
    Ok(path)
}

fn spawn(launch: &Launch) -> std::io::Result<()> {
    let mut command = std::process::Command::new(&launch.program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // No console flash for the helper; the terminal it opens is its own window.
        command.creation_flags(0x0800_0000);
        if launch.raw {
            command.raw_arg(launch.args.join(" "));
        } else {
            command.args(&launch.args);
        }
    }
    #[cfg(not(windows))]
    command.args(&launch.args);
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let mut child = command.spawn()?;
    // Reaped in the background; some terminals stay in front until closed.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

// MARK: - Commands

/// The result of Open in Terminal: either it opened `tool`, or `missing` says
/// which client to install and `install` how.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Opened {
    pub tool: Option<String>,
    pub missing: Option<String>,
    pub install: Option<String>,
}

/// Starts the client for a saved connection in a terminal. `session_id` is the
/// open session of that connection, whose SSH tunnel the terminal then shares.
#[tauri::command]
pub async fn open_in_terminal(
    state: State<'_, AppState>,
    connection_id: String,
    session_id: Option<String>,
) -> DbResult<Opened> {
    let (text, through_ssh) = state
        .with_store(|store| {
            let saved = store
                .get(&connection_id)
                .ok_or_else(|| DbError::msg("This saved connection is gone."))?;
            let url = store
                .url(&connection_id)
                .ok_or_else(|| DbError::msg("This connection's URL is missing."))?;
            Ok((url.to_string(), saved.ssh.is_some()))
        })
        .await?;
    let tunnel_port = session_id.as_deref().and_then(|id| state.tunnel_port(id));
    if through_ssh && tunnel_port.is_none() {
        return Err(DbError::msg(
            "This connection goes through SSH. Connect to it in dbLens, then open the terminal: it uses the same tunnel.",
        ));
    }
    let command = ClientCommand::make(&text, tunnel_port)?;
    let os = Os::current();
    let Some(tool) = resolve(&command.tools, &search_folders(os), os) else {
        return Ok(Opened {
            missing: Some(command.tools[0].to_string()),
            install: Some(command.install_hint(os).to_string()),
            ..Opened::default()
        });
    };
    let executable = tool.to_string_lossy().into_owned();
    let body = if os == Os::Windows {
        powershell_script(&executable, &command)
    } else {
        posix_script(&executable, &command)
    };
    let script = write_script(&body, os)?;
    let terminal = std::env::var("TERMINAL").ok();
    let result = terminal_launch(os, &script, terminal.as_deref(), &on_path)
        .ok_or_else(|| {
            DbError::msg(
                "No terminal program found. Install one (gnome-terminal, konsole, xterm…) or set $TERMINAL.",
            )
        })
        .and_then(|l| spawn(&l).map_err(|e| DbError::Message(format!("Open in Terminal: {e}"))));
    if let Err(error) = result {
        let _ = std::fs::remove_file(&script);
        return Err(error);
    }
    Ok(Opened {
        tool: tool.file_stem().map(|s| s.to_string_lossy().into_owned()),
        ..Opened::default()
    })
}

/// The saved URL with its password, for "Copy URL with Password". The page
/// asks the user first and clears the clipboard afterwards.
#[tauri::command]
pub async fn connection_url_with_password(
    state: State<'_, AppState>,
    id: String,
) -> DbResult<String> {
    state
        .with_store(|store| {
            store
                .url(&id)
                .map(String::from)
                .ok_or_else(|| DbError::msg("No such connection."))
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make(text: &str, port: Option<u16>) -> ClientCommand {
        ClientCommand::make(text, port).unwrap()
    }

    fn env(command: &ClientCommand, key: &str) -> Option<String> {
        command.environment.get(key).cloned()
    }

    #[test]
    fn postgres_keeps_the_password_out_of_the_arguments() {
        let c = make(
            "postgres://ana:s3cret@db.example.com:5433/shop?sslmode=require",
            None,
        );
        assert_eq!(c.tools, ["psql"]);
        assert_eq!(c.arguments.len(), 1);
        assert!(c.arguments[0].contains("host='db.example.com'"));
        assert!(c.arguments[0].contains("dbname='shop'"));
        assert!(!c.arguments[0].contains("s3cret"));
        assert_eq!(env(&c, "PGPASSWORD").as_deref(), Some("s3cret"));
    }

    #[test]
    fn postgres_through_a_tunnel_points_at_the_local_port() {
        let c = make("postgres://ana:pw@db.internal/shop", Some(40123));
        assert!(c.arguments[0].contains("host='127.0.0.1'"));
        assert!(c.arguments[0].contains("port='40123'"));
        assert!(!c.arguments[0].contains("db.internal"));
    }

    #[test]
    fn mysql_arguments() {
        let c = make(
            "mysql://root:pw@db.example.com:3307/shop?ssl-mode=required",
            None,
        );
        assert_eq!(c.tools, ["mysql", "mariadb"]);
        assert_eq!(
            c.arguments,
            [
                "--host=db.example.com",
                "--port=3307",
                "--protocol=TCP",
                "--user=root",
                "--ssl-mode=REQUIRED",
                "shop"
            ]
        );
        assert_eq!(env(&c, "MYSQL_PWD").as_deref(), Some("pw"));
        assert!(!c.arguments.iter().any(|a| a.contains("pw")));
    }

    #[test]
    fn mysql_defaults_socket_and_tunnel() {
        let plain = make("mysql://root@localhost/shop", None);
        assert_eq!(plain.arguments[0], "--host=localhost");
        assert_eq!(plain.arguments[1], "--port=3306");
        assert!(plain.environment.is_empty());
        let socket = make("mysql://root@localhost/shop?socket=/tmp/mysql.sock", None);
        assert_eq!(socket.arguments[0], "--socket=/tmp/mysql.sock");
        assert!(!socket.arguments.iter().any(|a| a.starts_with("--host")));
        // Through a tunnel the socket no longer applies.
        let tunnel = make("mysql://root@db/shop?socket=/tmp/mysql.sock", Some(5555));
        assert_eq!(
            tunnel.arguments[..3],
            ["--host=127.0.0.1", "--port=5555", "--protocol=TCP"]
        );
    }

    #[test]
    fn sqlite_opens_the_file() {
        let c = make("sqlite:///tmp/shop.db", None);
        assert_eq!(c.tools, ["sqlite3"]);
        assert_eq!(c.arguments, ["/tmp/shop.db"]);
        assert!(c.environment.is_empty());
    }

    #[test]
    fn redis_arguments() {
        let c = make(
            "rediss://ana:pw@cache.example.com:6380/3?ssl_cert_reqs=none",
            None,
        );
        assert_eq!(c.tools, ["redis-cli", "valkey-cli"]);
        assert_eq!(
            c.arguments,
            [
                "-h",
                "cache.example.com",
                "-p",
                "6380",
                "-n",
                "3",
                "--user",
                "ana",
                "--tls",
                "--insecure"
            ]
        );
        assert_eq!(env(&c, "REDISCLI_AUTH").as_deref(), Some("pw"));
        let plain = make("redis://localhost", Some(7000));
        assert_eq!(
            plain.arguments,
            ["-h", "127.0.0.1", "-p", "7000", "-n", "0"]
        );
    }

    #[test]
    fn mongo_drops_the_password_from_the_url() {
        let c = make(
            "mongodb://ana:s3cret@db.example.com:27017/shop?authSource=admin",
            None,
        );
        assert_eq!(c.tools, ["mongosh"]);
        assert_eq!(
            c.arguments,
            ["mongodb://ana@db.example.com:27017/shop?authSource=admin"]
        );
        assert!(c.environment.is_empty());
        let srv = make("mongodb+srv://ana:pw@cluster0.example.net/shop", None);
        assert_eq!(
            srv.arguments,
            ["mongodb+srv://ana@cluster0.example.net/shop"]
        );
    }

    #[test]
    fn mongo_through_a_tunnel() {
        let c = make(
            "mongodb+srv://an@a:p%40ss@cluster.example.net/shop?authSource=admin",
            Some(41000),
        );
        assert_eq!(
            c.arguments,
            ["mongodb://an%40a@127.0.0.1:41000/shop?authSource=admin&directConnection=true"]
        );
    }

    #[test]
    fn mysql_database_cannot_be_an_option() {
        assert!(ClientCommand::make("mysql://u:pw@h/--execute=system id", None).is_err());
        assert!(ClientCommand::make("mysql://u:pw@h/%2D%2Dexecute=x", None).is_err());
        let c = ClientCommand::make("mysql://u:pw@h/shop", None).unwrap();
        assert_eq!(c.arguments.last().unwrap(), "shop");
    }

    #[test]
    fn sqlite_dash_path_is_made_relative() {
        let c = ClientCommand::make("sqlite:-cmd", None).unwrap();
        assert!(c.arguments[0].starts_with("./") || !c.arguments[0].starts_with('-'));
    }

    #[test]
    fn mongo_keeps_no_secret_in_arguments() {
        assert!(ClientCommand::make("mongodb://u:pa/ss@h/db", None).is_err());
        let url = "mongodb://u:pw@h/db?tlsCertificateKeyFilePassword=SECRET&authSource=admin";
        let c = ClientCommand::make(url, None).unwrap();
        assert_eq!(c.arguments[0], "mongodb://u@h/db?authSource=admin");
    }

    #[test]
    fn strips_only_the_password() {
        assert_eq!(strip_password("mongodb://a:b@h/x"), "mongodb://a@h/x");
        assert_eq!(strip_password("mongodb://a@h/x"), "mongodb://a@h/x");
        assert_eq!(
            strip_password("mongodb://h:27017/x?u=a:b@c"),
            "mongodb://h:27017/x?u=a:b@c"
        );
        assert_eq!(strip_password("mongodb://a:p@ss@h/x"), "mongodb://a@h/x");
        assert_eq!(strip_password("no url"), "no url");
    }

    #[test]
    fn not_a_url_is_refused() {
        assert!(ClientCommand::make("hello", None).is_err());
    }

    #[test]
    fn posix_quoting() {
        assert_eq!(posix_quote("plain"), "'plain'");
        assert_eq!(posix_quote("it's"), "'it'\\''s'");
        assert_eq!(posix_quote("a b$c`d\"e\\f"), "'a b$c`d\"e\\f'");
        assert_eq!(posix_quote(""), "''");
    }

    #[test]
    fn powershell_quoting() {
        assert_eq!(powershell_quote("plain"), "'plain'");
        assert_eq!(powershell_quote("it's"), "'it''s'");
        assert_eq!(powershell_quote("a$b`c\"d"), "'a$b`c\"d'");
        assert_eq!(powershell_quote("\u{2019}x"), "'\u{2019}\u{2019}x'");
    }

    #[test]
    fn cmd_quoting() {
        assert_eq!(
            cmd_quote(r"C:\Temp\a b.ps1").as_deref(),
            Some(r#""C:\Temp\a b.ps1""#)
        );
        assert_eq!(cmd_quote("50%"), None);
        assert_eq!(cmd_quote("a\"b"), None);
        assert_eq!(cmd_quote("a\nb"), None);
    }

    #[test]
    fn posix_script_removes_itself_and_sets_the_environment() {
        let c = make("postgres://ana:it's@h/db", None);
        let script = posix_script("/usr/bin/psql", &c);
        let lines: Vec<&str> = script.lines().collect();
        assert_eq!(lines[0], "#!/bin/sh");
        assert_eq!(lines[1], "rm -f -- \"$0\"");
        assert_eq!(lines[2], "export PGPASSWORD='it'\\''s'");
        assert_eq!(lines[3], "clear");
        assert!(lines[4].starts_with("exec '/usr/bin/psql' '"));
        assert!(!lines[4].contains("it's"));
        assert!(script.ends_with('\n'));
    }

    #[cfg(unix)]
    #[test]
    fn the_posix_script_runs_deletes_itself_and_passes_the_password_by_environment() {
        let c = make("postgres://ana:it's a \"test\" $HOME@h/db", None);
        let path = write_script(&posix_script("/usr/bin/env", &c), Os::Linux).unwrap();
        let out = std::process::Command::new("sh")
            .arg(&path)
            .env("TERM", "dumb")
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("PGPASSWORD=it's a \"test\" $HOME"), "{text}");
        assert!(!path.exists(), "the script removes itself");
    }

    #[test]
    fn powershell_script_removes_itself_and_sets_the_environment() {
        let c = make("mysql://root:p'w@h/shop", None);
        let script = powershell_script(r"C:\Program Files\MySQL\bin\mysql.exe", &c);
        assert!(script.starts_with('\u{feff}'));
        let lines: Vec<&str> = script
            .trim_start_matches('\u{feff}')
            .split("\r\n")
            .collect();
        assert!(lines[0].starts_with("Remove-Item -LiteralPath $PSCommandPath"));
        assert_eq!(lines[1], "$env:MYSQL_PWD = 'p''w'");
        assert_eq!(lines[2], "Clear-Host");
        assert!(lines[3].starts_with(r"& 'C:\Program Files\MySQL\bin\mysql.exe' '--host=h'"));
    }

    #[test]
    fn finds_the_first_tool_in_the_first_folder() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        std::fs::write(b.path().join("mariadb"), "").unwrap();
        std::fs::write(a.path().join("mysql"), "").unwrap();
        let folders = vec![b.path().to_path_buf(), a.path().to_path_buf()];
        // mysql comes before mariadb in the tool list, in any folder.
        assert_eq!(
            resolve(&["mysql", "mariadb"], &folders, Os::Linux),
            Some(a.path().join("mysql"))
        );
        assert_eq!(resolve(&["mongosh"], &folders, Os::Linux), None);
        std::fs::write(a.path().join("psql.exe"), "").unwrap();
        assert_eq!(
            resolve(&["psql"], &folders, Os::Windows),
            Some(a.path().join("psql.exe"))
        );
        assert_eq!(resolve(&["psql"], &folders, Os::Linux), None);
    }

    fn only(names: &'static [&'static str]) -> impl Fn(&str) -> bool {
        move |n| names.contains(&n)
    }

    #[test]
    fn linux_terminals() {
        let script = Path::new("/tmp/dblens-1.sh");
        let l =
            terminal_launch(Os::Linux, script, None, &only(&["gnome-terminal", "xterm"])).unwrap();
        assert_eq!(l.program, "gnome-terminal");
        assert_eq!(l.args, ["--", "sh", "/tmp/dblens-1.sh"]);
        let l = terminal_launch(Os::Linux, script, None, &only(&["konsole"])).unwrap();
        assert_eq!((l.program.as_str(), l.args[0].as_str()), ("konsole", "-e"));
        let l = terminal_launch(Os::Linux, script, None, &only(&["kitty"])).unwrap();
        assert_eq!(l.args, ["sh", "/tmp/dblens-1.sh"]);
        let l = terminal_launch(
            Os::Linux,
            script,
            Some("wezterm-gui"),
            &only(&["wezterm-gui", "xterm"]),
        )
        .unwrap();
        assert_eq!(l.program, "wezterm-gui");
        assert!(terminal_launch(Os::Linux, script, None, &only(&[])).is_none());
    }

    #[test]
    fn windows_terminals() {
        let script = Path::new(r"C:\Users\ana\AppData\Local\Temp\dblens-1.ps1");
        let l = terminal_launch(Os::Windows, script, None, &only(&["wt.exe"])).unwrap();
        assert_eq!(l.program, "wt.exe");
        assert!(!l.raw);
        assert_eq!(l.args[0], "powershell.exe");
        assert_eq!(l.args.last().unwrap(), script.to_str().unwrap());
        let l = terminal_launch(Os::Windows, script, None, &only(&[])).unwrap();
        assert_eq!(l.program, "cmd.exe");
        assert!(l.raw);
        assert_eq!(l.args[..3], ["/c", "start", "\"dbLens\""]);
        assert_eq!(
            l.args.last().unwrap(),
            &format!("\"{}\"", script.to_str().unwrap())
        );
    }

    #[test]
    fn mac_terminal_runs_the_script_through_applescript() {
        let script = Path::new("/var/folders/ab/T/dblens-1.sh");
        let l = terminal_launch(Os::Mac, script, None, &only(&[])).unwrap();
        assert_eq!(l.program, "osascript");
        assert_eq!(
            l.args[1],
            "tell application \"Terminal\" to do script \"sh '/var/folders/ab/T/dblens-1.sh'\""
        );
        assert_eq!(applescript_string("a\"b\\c"), "a\\\"b\\\\c");
    }

    #[test]
    fn install_hints_exist_for_every_engine_and_os() {
        for engine in [
            Engine::Postgres,
            Engine::Mysql,
            Engine::Sqlite,
            Engine::Redis,
            Engine::Mongodb,
        ] {
            let c = ClientCommand {
                engine,
                tools: vec!["x"],
                arguments: vec![],
                environment: BTreeMap::new(),
            };
            for os in [Os::Mac, Os::Linux, Os::Windows] {
                assert!(!c.install_hint(os).is_empty());
            }
        }
    }
}
