//! An SSH tunnel through the system's own `ssh` (OpenSSH ships with Windows 10+
//! and every Linux): `ssh -N -L 127.0.0.1:<local>:<db host>:<db port>` with
//! BatchMode, so keys come from ssh-agent, a key file, or ~/.ssh/config and
//! there is never a password prompt. Port of `SSHTunnel.swift`.

use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};

use crate::session::{DbError, DbResult};

/// Where to tunnel through. Saved with a connection (not secret).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SshConfig {
    /// A host name, an IP, or a Host alias from ~/.ssh/config. May be user@host.
    pub host: String,
    /// None uses the port from ~/.ssh/config, or 22.
    pub port: Option<u16>,
    /// Empty uses the user from ~/.ssh/config, or the OS user name.
    pub user: String,
    /// A private key file. Empty uses ~/.ssh/config and ssh-agent.
    pub key_path: String,
}

impl SshConfig {
    /// "ana@bastion.example.com:2222" for the sidebar and the log.
    pub fn label(&self) -> String {
        let user = if self.user.is_empty() || self.host.contains('@') { String::new() } else { format!("{}@", self.user) };
        let port = self.port.map(|p| format!(":{p}")).unwrap_or_default();
        format!("{user}{}{port}", self.host)
    }
}

/// The arguments for ssh, public for tests.
pub fn arguments(config: &SshConfig, local_port: u16, remote_host: &str, remote_port: u16) -> Vec<String> {
    let target = if remote_host.contains(':') { format!("[{remote_host}]") } else { remote_host.to_string() };
    let mut args: Vec<String> = [
        "-N", "-T",
        "-L", &format!("127.0.0.1:{local_port}:{target}:{remote_port}"),
        "-o", "ExitOnForwardFailure=yes",
        "-o", "ServerAliveInterval=15",
        "-o", "ServerAliveCountMax=3",
        "-o", "ConnectTimeout=10",
        // No prompts: there is no terminal. Keys come from the agent or the key file.
        "-o", "BatchMode=yes",
        // Trust a server the first time, refuse it if its key ever changes.
        "-o", "StrictHostKeyChecking=accept-new",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if let Some(port) = config.port {
        args.extend(["-p".into(), port.to_string()]);
    }
    if !config.key_path.is_empty() {
        args.extend(["-i".into(), expand_tilde(&config.key_path), "-o".into(), "IdentitiesOnly=yes".into()]);
    }
    if !config.user.is_empty() && !config.host.contains('@') {
        args.extend(["-l".into(), config.user.clone()]);
    }
    // "--": nothing after it is read as an option, whatever the host says.
    args.extend(["--".into(), config.host.clone()]);
    args
}

fn expand_tilde(path: &str) -> String {
    match path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        Some(rest) => directories::BaseDirs::new()
            .map(|d| d.home_dir().join(rest).to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string()),
        None => path.to_string(),
    }
}

/// Why these values can't go to ssh, or None. A host like "-oProxyCommand=…"
/// would run a command, and shell characters reach a ProxyCommand's %h.
pub fn problem(config: &SshConfig, remote_host: &str) -> Option<String> {
    let host_ok = |c: char| c.is_ascii_alphanumeric() || ".-_@:[]%".contains(c);
    let user_ok = |c: char| c.is_ascii_alphanumeric() || ".-_".contains(c);
    let valid = |v: &str, ok: &dyn Fn(char) -> bool| !v.starts_with('-') && v.chars().all(ok);
    if config.host.is_empty() || !valid(&config.host, &host_ok) {
        return Some("The SSH server can only have letters, digits, and . - _ @ : [ ] (and can't start with -).".into());
    }
    if !config.user.is_empty() && !valid(&config.user, &user_ok) {
        return Some("The SSH user can only have letters, digits, and . - _ (and can't start with -).".into());
    }
    if config.key_path.starts_with('-') || config.key_path.contains(['\n', '\r']) {
        return Some("The SSH key path is not valid.".into());
    }
    if remote_host.is_empty() || !valid(remote_host, &host_ok) {
        return Some("The database host is not valid for an SSH tunnel.".into());
    }
    None
}

/// A running tunnel. Dropping it (or `close`) stops ssh.
pub struct SshTunnel {
    pub local_port: u16,
    pub config: SshConfig,
    child: Mutex<Child>,
    errors: Arc<Mutex<String>>,
}

impl std::fmt::Debug for SshTunnel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SshTunnel").field("local_port", &self.local_port).field("config", &self.config).finish()
    }
}

impl SshTunnel {
    /// Opens the tunnel and waits until the local port answers, or fails with ssh's own message.
    pub async fn open(config: &SshConfig, remote_host: &str, remote_port: u16) -> DbResult<SshTunnel> {
        Self::open_with(&ssh_program(), config, remote_host, remote_port).await
    }

    /// `open` with an explicit ssh program (tests use a stand-in).
    async fn open_with(program: &str, config: &SshConfig, remote_host: &str, remote_port: u16) -> DbResult<SshTunnel> {
        if let Some(problem) = problem(config, remote_host) {
            return Err(DbError::Message(problem));
        }
        let local_port = free_port()?;
        let mut command = Command::new(program);
        command
            .args(arguments(config, local_port, remote_host, remote_port))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        {
            // CREATE_NO_WINDOW: no console flashes up for ssh.exe.
            command.creation_flags(0x0800_0000);
        }
        let mut child = command.spawn().map_err(|e| DbError::Message(format!("Couldn't start ssh: {e}")))?;
        let errors = Arc::new(Mutex::new(String::new()));
        if let Some(mut stderr) = child.stderr.take() {
            let sink = errors.clone();
            tokio::spawn(async move {
                let mut buffer = [0u8; 1024];
                while let Ok(n) = stderr.read(&mut buffer).await {
                    if n == 0 {
                        break;
                    }
                    if let Ok(mut text) = sink.lock() {
                        text.push_str(&String::from_utf8_lossy(&buffer[..n]));
                    }
                }
            });
        }
        let tunnel = SshTunnel { local_port, config: config.clone(), child: Mutex::new(child), errors };
        tunnel.wait_until_ready().await?;
        Ok(tunnel)
    }

    /// Polls the local port; ssh opens it only after logging in and setting up the forward.
    async fn wait_until_ready(&self) -> DbResult<()> {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if !self.is_running() {
                // Let the stderr reader catch up.
                tokio::time::sleep(Duration::from_millis(100)).await;
                return Err(DbError::Message(format!("SSH: {}", self.error_text())));
            }
            if tokio::net::TcpStream::connect(("127.0.0.1", self.local_port)).await.is_ok() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        self.close();
        Err(DbError::msg("SSH: the tunnel did not respond within 20 seconds."))
    }

    /// Why the tunnel is gone, or None while it runs. For "the tunnel dropped" errors.
    pub fn drop_reason(&self) -> Option<String> {
        if self.is_running() {
            None
        } else {
            Some(format!("The SSH tunnel to {} dropped: {}", self.config.label(), self.error_text()))
        }
    }

    pub fn is_running(&self) -> bool {
        self.child.lock().map(|mut c| matches!(c.try_wait(), Ok(None))).unwrap_or(false)
    }

    /// What ssh said, without the "Permanently added" noise.
    pub fn error_text(&self) -> String {
        let text = self.errors.lock().map(|t| t.clone()).unwrap_or_default();
        let text = text
            .lines()
            .filter(|l| !l.starts_with("Warning: Permanently added"))
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string();
        if text.is_empty() {
            "ssh ended without a message.".into()
        } else {
            text
        }
    }

    /// Stops the tunnel. Safe to call more than once.
    pub fn close(&self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.start_kill();
        }
    }
}

impl Drop for SshTunnel {
    fn drop(&mut self) {
        self.close();
    }
}

/// `ssh` from PATH; on Windows the built-in OpenSSH when PATH lacks it.
fn ssh_program() -> String {
    #[cfg(windows)]
    {
        let system = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        let builtin = format!("{system}\\System32\\OpenSSH\\ssh.exe");
        if std::path::Path::new(&builtin).exists() {
            return builtin;
        }
    }
    "ssh".into()
}

/// Asks the system for an unused port by binding to port 0.
fn free_port() -> DbResult<u16> {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .map_err(|_| DbError::msg("No free local port for the tunnel."))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(host: &str, user: &str, key: &str) -> SshConfig {
        SshConfig { host: host.into(), port: None, user: user.into(), key_path: key.into() }
    }

    #[test]
    fn builds_arguments() {
        let config = SshConfig { host: "bastion.x.com".into(), port: Some(2222), user: "ana".into(), key_path: String::new() };
        let args = arguments(&config, 61000, "db.internal", 5432);
        assert_eq!(&args[args.len() - 4..], ["-l", "ana", "--", "bastion.x.com"]);
        assert!(args.contains(&"BatchMode=yes".to_string()) && args.contains(&"2222".to_string()));
        assert!(arguments(&cfg("bastion", "", ""), 1, "::1", 5432).contains(&"127.0.0.1:1:[::1]:5432".to_string()));
        assert_eq!(config.label(), "ana@bastion.x.com:2222");
    }

    #[test]
    fn refuses_dangerous_values() {
        for config in [
            cfg("-oProxyCommand=calc", "", ""),
            cfg("a;touch /tmp/x", "", ""),
            cfg("bastion", "-oProxyCommand=x", ""),
            cfg("bastion", "ana bob", ""),
            cfg("bastion", "", "-oX"),
        ] {
            assert!(problem(&config, "localhost").is_some(), "{config:?}");
        }
        for host in ["bastion", "ana@db.x.com", "100.64.0.1", "[::1]", "my-host_1.example.com"] {
            assert!(problem(&cfg(host, "ana.b-1", "~/.ssh/id_ed25519"), "db-service-internal").is_none(), "{host}");
        }
        assert!(problem(&cfg("bastion", "", ""), "db;rm").is_some());
    }

    #[tokio::test]
    async fn reports_ssh_errors() {
        let config = SshConfig { host: "127.0.0.1".into(), port: Some(1), ..Default::default() };
        // Fails whether or not ssh is installed: no server on port 1.
        assert!(SshTunnel::open(&config, "localhost", 5432).await.is_err());
    }

    #[cfg(unix)]
    fn fake_ssh(dir: &std::path::Path, body: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("fake-ssh");
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[cfg(unix)]
    use std::fs;

    #[cfg(unix)]
    #[tokio::test]
    async fn surfaces_the_message_of_a_failing_ssh() {
        let dir = tempfile::tempdir().unwrap();
        let program = fake_ssh(dir.path(), "echo 'Warning: Permanently added x' >&2; echo 'Permission denied (publickey).' >&2; exit 255");
        let error = SshTunnel::open_with(&program, &cfg("bastion", "", ""), "db", 5432).await.unwrap_err();
        assert_eq!(error.to_string(), "SSH: Permission denied (publickey).");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn opens_waits_detects_drop_and_kills() {
        if std::process::Command::new("python3").arg("--version").output().is_err() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        // Stand-in for ssh: listens on the -L local port like a real forward would.
        let script = "exec python3 -c \"import socket,sys,time\nspec=sys.argv[sys.argv.index('-L')+1].split(':')\ns=socket.socket();s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1);s.bind((spec[0],int(spec[1])));s.listen(5);time.sleep(60)\" \"$@\"";
        let program = fake_ssh(dir.path(), script);
        let tunnel = SshTunnel::open_with(&program, &cfg("bastion", "", ""), "db", 5432).await.unwrap();
        assert!(tunnel.is_running() && tunnel.drop_reason().is_none());
        assert!(tokio::net::TcpStream::connect(("127.0.0.1", tunnel.local_port)).await.is_ok());
        tunnel.close();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!tunnel.is_running());
        assert!(tunnel.drop_reason().unwrap().starts_with("The SSH tunnel to bastion dropped"));
        // The port is free again once ssh is gone.
        assert!(tokio::net::TcpStream::connect(("127.0.0.1", tunnel.local_port)).await.is_err());
    }
}
