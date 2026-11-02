//! Settings > Agents: dbLens's MCP server (`dblens --mcp`, mcp.rs) added to,
//! or removed from, the apps that run MCP servers, with one click (the same
//! list and rules as AgentClient in UI/AgentSettings.swift).
//!
//! Each app keeps its servers in a config file of its own: JSON under a key
//! (`mcpServers`, `servers`, `context_servers`) or TOML (`[mcp_servers.dblens]`).
//! Only the `dblens` entry is touched; the rest of the file stays as it was.
//! Before a changed file is written, the old one is copied to
//! `<file>.dblens-backup`. Claude Code goes through its own `claude mcp`
//! command when it is installed, since its settings file holds much more.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// An app that runs MCP servers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentClient {
    ClaudeCode,
    ClaudeDesktop,
    /// ChatGPT's Codex: the app and the `codex` command.
    Codex,
    Cursor,
    /// Devin Desktop, formerly Windsurf.
    Windsurf,
    VsCode,
    GeminiCli,
    GrokCli,
    Zed,
}

pub const CLIENTS: [AgentClient; 9] = [
    AgentClient::ClaudeCode,
    AgentClient::ClaudeDesktop,
    AgentClient::Codex,
    AgentClient::Cursor,
    AgentClient::Windsurf,
    AgentClient::VsCode,
    AgentClient::GeminiCli,
    AgentClient::GrokCli,
    AgentClient::Zed,
];

/// Where one app stands, for the Agents tab.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentStatus {
    pub id: AgentClient,
    /// The app (or its command) is on this computer.
    pub available: bool,
    /// Its config has a `dblens` entry.
    pub installed: bool,
    /// The entry runs this copy of dbLens (not one moved or deleted since).
    pub current: bool,
    /// What the entry runs; empty when it has no `command`.
    pub installed_command: Option<String>,
    /// The file the entry lives in.
    pub config_file: String,
}

/// One MCP tool, for Settings > Agents > Tools.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTool {
    pub name: String,
    /// The first sentence of the tool's description.
    pub summary: String,
    /// The tool changes data (it works only on connections that allow it).
    pub writes: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Format {
    /// JSON with the servers under this key.
    Json(&'static str),
    Toml,
}

impl AgentClient {
    fn format(self) -> Format {
        match self {
            AgentClient::Codex | AgentClient::GrokCli => Format::Toml,
            AgentClient::VsCode => Format::Json("servers"),
            AgentClient::Zed => Format::Json("context_servers"),
            _ => Format::Json("mcpServers"),
        }
    }

    /// dbLens's entry in this app's JSON format.
    fn entry(self, exe: &str) -> Value {
        match self {
            AgentClient::VsCode => json!({"type": "stdio", "command": exe, "args": ["--mcp"]}),
            _ => json!({"command": exe, "args": ["--mcp"]}),
        }
    }

    /// The app's config file on this system.
    fn config_file(self) -> Option<PathBuf> {
        let dirs = directories::BaseDirs::new()?;
        let home = dirs.home_dir();
        // macOS: ~/Library/Application Support; Windows: %APPDATA%; Linux: ~/.config.
        let config = dirs.config_dir();
        Some(match self {
            AgentClient::ClaudeCode => home.join(".claude.json"),
            AgentClient::ClaudeDesktop => config.join("Claude").join("claude_desktop_config.json"),
            AgentClient::Codex => home.join(".codex").join("config.toml"),
            AgentClient::Cursor => home.join(".cursor").join("mcp.json"),
            AgentClient::Windsurf => home
                .join(".codeium")
                .join("windsurf")
                .join("mcp_config.json"),
            AgentClient::VsCode => config.join("Code").join("User").join("mcp.json"),
            AgentClient::GeminiCli => home.join(".gemini").join("settings.json"),
            AgentClient::GrokCli => home.join(".grok").join("config.toml"),
            AgentClient::Zed => {
                if cfg!(windows) {
                    config.join("Zed").join("settings.json")
                } else {
                    home.join(".config").join("zed").join("settings.json")
                }
            }
        })
    }

    /// The command that starts the app from a terminal, when it has one.
    fn binary_name(self) -> Option<&'static str> {
        match self {
            AgentClient::ClaudeCode => Some("claude"),
            AgentClient::Codex => Some("codex"),
            AgentClient::GeminiCli => Some("gemini"),
            AgentClient::GrokCli => Some("grok"),
            AgentClient::Cursor => Some("cursor"),
            AgentClient::Windsurf => Some("windsurf"),
            AgentClient::VsCode => Some("code"),
            AgentClient::Zed => Some("zed"),
            AgentClient::ClaudeDesktop => None,
        }
    }

    /// macOS app bundle names.
    fn app_names(self) -> &'static [&'static str] {
        match self {
            AgentClient::ClaudeCode | AgentClient::ClaudeDesktop => &["Claude"],
            AgentClient::Codex => &["ChatGPT", "Codex"],
            AgentClient::Cursor => &["Cursor"],
            AgentClient::Windsurf => &["Devin", "Windsurf"],
            AgentClient::VsCode => &["Visual Studio Code"],
            AgentClient::Zed => &["Zed"],
            AgentClient::GeminiCli | AgentClient::GrokCli => &[],
        }
    }

    /// The app (or its command) is on this computer. The command-line tools
    /// need their command; the others also count when their config folder
    /// exists, since the app has been run.
    fn available(self) -> bool {
        if self
            .binary_name()
            .is_some_and(|name| find_binary(name).is_some())
        {
            return true;
        }
        if cfg!(target_os = "macos") && self.app_names().iter().any(|n| mac_app_exists(n)) {
            return true;
        }
        match self {
            AgentClient::GeminiCli | AgentClient::GrokCli => false,
            _ => self
                .config_file()
                .is_some_and(|file| file.exists() || file.parent().is_some_and(Path::is_dir)),
        }
    }
}

// MARK: Finding apps

/// Where command-line tools live, besides the PATH.
const EXTRA_BIN_DIRS: [&str; 6] = [
    ".local/bin",
    ".claude/local",
    ".npm-global/bin",
    ".bun/bin",
    "/opt/homebrew/bin",
    "/usr/local/bin",
];

fn find_binary(name: &str) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    let home = directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf());
    for extra in EXTRA_BIN_DIRS {
        if extra.starts_with('/') {
            dirs.push(PathBuf::from(extra));
        } else if let Some(home) = &home {
            dirs.push(home.join(extra));
        }
    }
    let names: Vec<String> = if cfg!(windows) {
        vec![
            format!("{name}.exe"),
            format!("{name}.cmd"),
            name.to_string(),
        ]
    } else {
        vec![name.to_string()]
    };
    dirs.iter()
        .flat_map(|dir| names.iter().map(move |n| dir.join(n)))
        .find(|path| is_executable(path))
}

fn is_executable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn mac_app_exists(name: &str) -> bool {
    let home = directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf());
    Path::new(&format!("/Applications/{name}.app")).exists()
        || home.is_some_and(|h| h.join("Applications").join(format!("{name}.app")).exists())
}

// MARK: Reading and editing the config text

/// TOML basic strings need `\` and `"` escaped (Windows paths have backslashes).
fn toml_quote(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

fn toml_unquote(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else {
            out.push(c);
        }
    }
    out
}

const TOML_TABLE: &str = "[mcp_servers.dblens]";

/// The command the `dblens` entry runs: `Some("")` when it has none, `None`
/// when there is no entry (or the text can't be read).
pub fn installed_command(format_of: AgentClient, text: &str) -> Option<String> {
    match format_of.format() {
        Format::Toml => {
            let mut in_table = false;
            for line in text.lines().map(str::trim) {
                if line.starts_with('[') {
                    in_table = line == TOML_TABLE;
                    continue;
                }
                if in_table && line.starts_with("command") {
                    if let (Some(start), Some(end)) = (line.find('"'), line.rfind('"')) {
                        if start < end {
                            return Some(toml_unquote(&line[start + 1..end]));
                        }
                    }
                }
            }
            text.lines()
                .any(|l| l.trim() == TOML_TABLE)
                .then(String::new)
        }
        Format::Json(key) => {
            let root: Value = serde_json::from_str(text).ok()?;
            let entry = root.get(key)?.get("dblens")?;
            Some(
                entry
                    .get("command")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            )
        }
    }
}

/// The text without one `[table]` and its keys (up to the next table).
fn removing_toml_table(name: &str, text: &str) -> String {
    let header = format!("[{name}]");
    let mut kept: Vec<&str> = Vec::new();
    let mut skipping = false;
    for line in text.split('\n') {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            skipping = trimmed == header;
        }
        if !skipping {
            kept.push(line);
        }
    }
    kept.join("\n").replace("\n\n\n", "\n\n")
}

/// The config text with the `dblens` entry added (or replaced) or removed, and
/// everything else as it was. JSON is read as plain JSON: a file with
/// comments is refused, never rewritten.
pub fn edited(client: AgentClient, text: &str, exe: &str, install: bool) -> Result<String, String> {
    match client.format() {
        Format::Toml => {
            let mut out = removing_toml_table("mcp_servers.dblens", text);
            if install {
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                out.push_str(&format!(
                    "\n{TOML_TABLE}\ncommand = {}\nargs = [\"--mcp\"]\n",
                    toml_quote(exe)
                ));
            }
            Ok(out)
        }
        Format::Json(key) => {
            let mut root: Value = if text.trim().is_empty() {
                json!({})
            } else {
                serde_json::from_str(text)
                    .ok()
                    .filter(Value::is_object)
                    .ok_or_else(|| {
                        "has comments or isn't plain JSON; add dbLens by hand with Copy JSON."
                            .to_string()
                    })?
            };
            let object = root.as_object_mut().expect("checked to be an object");
            let servers = object.entry(key).or_insert_with(|| json!({}));
            if !servers.is_object() {
                return Err(format!(
                    "\"{key}\" isn't an object; add dbLens by hand with Copy JSON."
                ));
            }
            let servers = servers.as_object_mut().expect("checked to be an object");
            if install {
                servers.insert("dblens".into(), client.entry(exe));
            } else {
                servers.remove("dblens");
            }
            let mut out = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;
            out.push('\n');
            Ok(out)
        }
    }
}

// MARK: Files

/// Adds or removes the entry in a config file: creates it (and its folder)
/// when missing, copies the old one to `<file>.dblens-backup` first, and
/// writes through a temporary file so a crash never leaves half a config.
pub fn edit_file(client: AgentClient, file: &Path, exe: &str, install: bool) -> Result<(), String> {
    let current = match std::fs::read_to_string(file) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("{}: {e}", file.display())),
    };
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let next = edited(client, current.as_deref().unwrap_or(""), exe, install)
        .map_err(|e| format!("{name} {e}").replace(&format!("{name} {name}"), &name))?;
    if current.as_deref() == Some(next.as_str()) {
        return Ok(());
    }
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    if current.is_some() {
        let mut backup = file.as_os_str().to_owned();
        backup.push(".dblens-backup");
        std::fs::copy(file, PathBuf::from(backup))
            .map_err(|e| format!("{}: {e}", file.display()))?;
    }
    let mut temporary = file.as_os_str().to_owned();
    temporary.push(".dblens-tmp");
    let temporary = PathBuf::from(temporary);
    std::fs::write(&temporary, next).map_err(|e| format!("{}: {e}", temporary.display()))?;
    std::fs::rename(&temporary, file).map_err(|e| {
        let _ = std::fs::remove_file(&temporary);
        format!("{}: {e}", file.display())
    })
}

/// This program, which the apps will start with `--mcp`.
pub fn executable() -> String {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "dblens".into())
}

pub fn status(client: AgentClient, exe: &str) -> AgentStatus {
    let file = client.config_file();
    let command = file
        .as_ref()
        .and_then(|f| std::fs::read_to_string(f).ok())
        .and_then(|text| installed_command(client, &text));
    AgentStatus {
        id: client,
        available: client.available(),
        installed: command.is_some(),
        current: command.as_deref() == Some(exe),
        installed_command: command,
        config_file: file
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default(),
    }
}

pub fn all_statuses() -> Vec<AgentStatus> {
    let exe = executable();
    CLIENTS.iter().map(|c| status(*c, &exe)).collect()
}

fn run_claude(claude: &Path, args: &[&str]) -> Result<(), String> {
    let output = std::process::Command::new(claude)
        .args(args)
        .output()
        .map_err(|e| format!("{}: {e}", claude.display()))?;
    if output.status.success() {
        return Ok(());
    }
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    Err(text.trim().to_string())
}

/// Installs or removes dbLens in one app.
pub fn set_installed(client: AgentClient, install: bool) -> Result<(), String> {
    let exe = executable();
    if client == AgentClient::ClaudeCode {
        if let Some(claude) = find_binary("claude") {
            // An older entry is replaced, so "remove" first and ignore its failure.
            let _ = run_claude(&claude, &["mcp", "remove", "--scope", "user", "dblens"]);
            if install {
                run_claude(
                    &claude,
                    &[
                        "mcp", "add", "--scope", "user", "dblens", "--", &exe, "--mcp",
                    ],
                )?;
            }
            return Ok(());
        }
    }
    let file = client
        .config_file()
        .ok_or("The home folder wasn't found.")?;
    edit_file(client, &file, &exe, install)
}

// MARK: Tools

/// The MCP tools, from the server's own list (mcp.rs), for the Tools section.
pub fn tools_for_settings() -> Vec<AgentTool> {
    let list = crate::mcp::tool_list();
    list.as_array()
        .map(|tools| {
            tools
                .iter()
                .map(|tool| {
                    let description = tool["description"].as_str().unwrap_or("");
                    let summary = description
                        .find(". ")
                        .map(|i| format!("{}.", &description[..i]))
                        .unwrap_or_else(|| description.to_string());
                    AgentTool {
                        name: tool["name"].as_str().unwrap_or("").to_string(),
                        summary,
                        writes: tool["annotations"]["readOnlyHint"] == json!(false),
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

// MARK: Commands

#[tauri::command]
pub async fn agent_clients() -> Vec<AgentStatus> {
    tokio::task::spawn_blocking(all_statuses)
        .await
        .unwrap_or_default()
}

/// Installs or removes dbLens in one app, then the new state of all of them.
#[tauri::command]
pub async fn set_agent_installed(
    client: AgentClient,
    installed: bool,
) -> Result<Vec<AgentStatus>, String> {
    tokio::task::spawn_blocking(move || set_installed(client, installed).map(|()| all_statuses()))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn agent_tools() -> Vec<AgentTool> {
    tools_for_settings()
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXE: &str = "/Applications/dbLens.app/Contents/MacOS/dbLens";

    #[test]
    fn json_entry_is_added_and_removed_keeping_the_rest() {
        let text = r#"{"theme": "dark", "mcpServers": {"other": {"command": "x"}}}"#;
        let added = edited(AgentClient::Cursor, text, EXE, true).unwrap();
        let value: Value = serde_json::from_str(&added).unwrap();
        assert_eq!(value["theme"], "dark");
        assert_eq!(value["mcpServers"]["other"]["command"], "x");
        assert_eq!(value["mcpServers"]["dblens"]["command"], EXE);
        assert_eq!(value["mcpServers"]["dblens"]["args"], json!(["--mcp"]));
        assert_eq!(
            installed_command(AgentClient::Cursor, &added).as_deref(),
            Some(EXE)
        );

        let removed = edited(AgentClient::Cursor, &added, EXE, false).unwrap();
        let value: Value = serde_json::from_str(&removed).unwrap();
        assert_eq!(value["mcpServers"]["other"]["command"], "x");
        assert!(value["mcpServers"].get("dblens").is_none());
        assert_eq!(installed_command(AgentClient::Cursor, &removed), None);
    }

    #[test]
    fn each_app_uses_its_own_key_and_entry() {
        let vs = edited(AgentClient::VsCode, "", EXE, true).unwrap();
        let value: Value = serde_json::from_str(&vs).unwrap();
        assert_eq!(value["servers"]["dblens"]["type"], "stdio");
        let zed = edited(AgentClient::Zed, "", EXE, true).unwrap();
        let value: Value = serde_json::from_str(&zed).unwrap();
        assert_eq!(value["context_servers"]["dblens"]["command"], EXE);
        assert!(value["context_servers"]["dblens"].get("type").is_none());
    }

    #[test]
    fn a_file_with_comments_is_refused_not_rewritten() {
        let text = "{\n  // my settings\n  \"theme\": \"dark\"\n}";
        let error = edited(AgentClient::Zed, text, EXE, true).unwrap_err();
        assert!(error.contains("isn't plain JSON"), "{error}");
        assert!(edited(AgentClient::Zed, "[1, 2]", EXE, true).is_err());
    }

    #[test]
    fn toml_entry_is_added_replaced_and_removed() {
        let text = "model = \"gpt\"\n\n[mcp_servers.other]\ncommand = \"x\"\n";
        let added = edited(AgentClient::Codex, text, EXE, true).unwrap();
        assert!(added.contains("model = \"gpt\""));
        assert!(added.contains("[mcp_servers.other]\ncommand = \"x\""));
        assert!(added.contains("[mcp_servers.dblens]\ncommand = \"/Applications/dbLens.app/Contents/MacOS/dbLens\"\nargs = [\"--mcp\"]"));
        assert_eq!(
            installed_command(AgentClient::Codex, &added).as_deref(),
            Some(EXE)
        );

        // Installing again replaces the entry instead of repeating it.
        let again = edited(AgentClient::Codex, &added, "/new/dblens", true).unwrap();
        assert_eq!(again.matches("[mcp_servers.dblens]").count(), 1);
        assert_eq!(
            installed_command(AgentClient::Codex, &again).as_deref(),
            Some("/new/dblens")
        );

        let removed = edited(AgentClient::Codex, &again, EXE, false).unwrap();
        assert!(removed.contains("[mcp_servers.other]"));
        assert_eq!(installed_command(AgentClient::Codex, &removed), None);
    }

    #[test]
    fn toml_keeps_windows_paths_intact() {
        let exe = r"C:\Program Files\dbLens\dblens.exe";
        let added = edited(AgentClient::GrokCli, "", exe, true).unwrap();
        assert!(
            added.contains(r#"command = "C:\\Program Files\\dbLens\\dblens.exe""#),
            "{added}"
        );
        assert_eq!(
            installed_command(AgentClient::GrokCli, &added).as_deref(),
            Some(exe)
        );
    }

    #[test]
    fn editing_a_file_makes_a_backup_and_leaves_no_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("sub").join("mcp.json");
        // A missing file and folder are created.
        edit_file(AgentClient::Cursor, &file, EXE, true).unwrap();
        assert_eq!(
            installed_command(
                AgentClient::Cursor,
                &std::fs::read_to_string(&file).unwrap()
            )
            .as_deref(),
            Some(EXE)
        );
        assert!(!dir
            .path()
            .join("sub")
            .join("mcp.json.dblens-backup")
            .exists());

        let before = std::fs::read_to_string(&file).unwrap();
        edit_file(AgentClient::Cursor, &file, EXE, false).unwrap();
        let backup = dir.path().join("sub").join("mcp.json.dblens-backup");
        assert_eq!(std::fs::read_to_string(backup).unwrap(), before);
        assert!(!dir.path().join("sub").join("mcp.json.dblens-tmp").exists());
        assert_eq!(
            installed_command(
                AgentClient::Cursor,
                &std::fs::read_to_string(&file).unwrap()
            ),
            None
        );
    }

    #[test]
    fn a_refused_file_is_left_exactly_as_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.json");
        let text = "{ // comment\n \"a\": 1 }";
        std::fs::write(&file, text).unwrap();
        let error = edit_file(AgentClient::Zed, &file, EXE, true).unwrap_err();
        assert!(error.starts_with("settings.json has comments"), "{error}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), text);
    }

    #[test]
    fn the_tools_list_marks_the_ones_that_write() {
        let tools = tools_for_settings();
        assert!(tools
            .iter()
            .any(|t| t.name == "list_connections" && !t.writes));
        assert!(tools
            .iter()
            .any(|t| t.name == "install_extension" && t.writes));
        assert!(tools
            .iter()
            .all(|t| !t.summary.is_empty() && !t.summary.contains(". ")));
    }

    #[test]
    fn a_stale_entry_is_not_current() {
        let text = edited(AgentClient::Cursor, "", "/old/dblens", true).unwrap();
        let command = installed_command(AgentClient::Cursor, &text);
        assert_eq!(command.as_deref(), Some("/old/dblens"));
        assert_ne!(command.as_deref(), Some(EXE));
    }
}
