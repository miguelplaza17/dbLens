//! Why a connection failed, in one clear sentence, with the server's own
//! message under it. Port of `ConnectionProblem.swift` ("this Mac" reads
//! "this computer"). Messages are English; the frontend may translate the
//! headline by key where it matches `Localization/*.json`.

use std::sync::OnceLock;

use regex::Regex;

use crate::connector::Summary;
use crate::engine::Engine;
use crate::session::DbError;

/// The error to show for a failed connect: the cause first, then the server's message.
pub fn explain(error: &DbError, url: &str, via_ssh: bool) -> DbError {
    let raw = error.to_string();
    let detail = clean(&raw);
    match headline(&raw, crate::connector::summary(url).as_ref(), url, via_ssh) {
        Some(headline) => DbError::Message(format!("{headline}\n\n{detail}")),
        None => DbError::Message(detail),
    }
}

fn compose(headline: String, hints: Vec<String>) -> String {
    std::iter::once(headline).chain(hints.into_iter().map(|h| format!("• {h}"))).collect::<Vec<_>>().join("\n")
}

fn default_port(engine: Engine) -> u16 {
    engine.default_port().unwrap_or(0)
}

pub fn headline(raw: &str, summary: Option<&Summary>, text: &str, via_ssh: bool) -> Option<String> {
    let message = raw.to_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| message.contains(&n.to_lowercase()));
    // Mongo's host may carry its port ("db:27017"); one host is enough for the sentence.
    let mut host = summary.and_then(|s| s.host.clone()).unwrap_or_else(|| "localhost".into());
    if summary.and_then(|s| s.port).is_none() && !host.contains(']') {
        if let Some(colon) = host.rfind(':') {
            host.truncate(colon);
        }
    }
    let place = match summary.and_then(|s| s.port) {
        Some(port) => format!("{host}:{port}"),
        None => host.clone(),
    };
    let user = summary.and_then(|s| s.user.clone()).unwrap_or_default();
    let database = summary.map(|s| s.database.replace("db ", "")).unwrap_or_default();
    let tunnel = if via_ssh { " (through the SSH tunnel)" } else { "" };
    let lower_text = text.to_lowercase();
    let tls_url = lower_text.starts_with("rediss://") || has(&["sslmode=require", "tls=true", "ssl=true"]);

    if has(&["ssl", "tls"]) && has(&["not support", "não oferece suporte", "does not support", "wrong version", "handshake", "requires"]) {
        return Some(format!("TLS didn't work with {place}: the server and the URL disagree on encryption. Check the URL's TLS/SSL setting."));
    }
    let local = ["localhost", "127.0.0.1", "::1"].contains(&host.as_str());
    let default_port = summary.map(|s| default_port(s.engine)).unwrap_or(0);
    let docker = "If the database runs in Docker, publish its port on the server (ports: in Docker Compose, or the External Port field in panels like Dokploy) and put that port in the URL.".to_string();
    let ssh_host = "Through SSH, the URL's host and port are seen from the SSH server: use the address the database has there (127.0.0.1 and a published port, or the container's IP), not this computer's.".to_string();
    // Connection refused: macOS errno 61, Linux 111, Windows 10061.
    if has(&["refused", "(61)", "(36)", "10061", "os error 111"]) {
        let mut hints = Vec::new();
        if local && !via_ssh {
            hints.push("Is the database running on this computer?".to_string());
        } else if !via_ssh {
            hints.push(docker);
        }
        if via_ssh {
            hints.push(ssh_host.clone());
        }
        if let Some(port) = summary.and_then(|s| s.port) {
            if default_port > 0 && port != default_port {
                hints.push(format!("The URL uses port {port}; this database's usual port is {default_port}."));
            }
        }
        return Some(compose(format!("Nothing answered at {place}{tunnel}: the connection was refused."), hints));
    }
    if has(&["invalid srv service name"]) {
        return Some(format!("\"{host}\" isn't a valid cluster name for mongodb+srv://. Check the URL's host."));
    }
    if has(&[
        "could not translate host", "traduzir o nome", "unknown server host", "nosuchrecord", "failed to resolve",
        "nodename nor servname", "failed to look up srv", "host not found", "name or service not known",
        "failed to lookup address", "no such host", "temporary failure in name resolution",
    ]) {
        // A name without a dot (a Docker service, a LAN machine) only resolves inside its own network.
        if !host.contains('.') && !host.contains(':') && host != "localhost" {
            let hint = if via_ssh {
                "Through SSH the name must resolve on the SSH server, and container names usually don't resolve on the server itself: use the container's IP, or 127.0.0.1 and a published port."
            } else {
                "From this computer, use the server's IP or name instead, with the database's published port (ports: in Docker Compose, or the External Port field in panels like Dokploy), or connect through SSH to that server."
            };
            return Some(compose(
                format!("The host \"{host}\" wasn't found. It looks like a container or service name, which only resolves inside its own Docker network."),
                vec![hint.into()],
            ));
        }
        return Some(compose(
            format!("The host \"{host}\" wasn't found."),
            vec!["Check the name, and whether this computer can reach that network (some need a VPN).".into()],
        ));
    }
    if has(&["timeout", "tempo limite", "timed out", "(60)", "within", "10060"]) {
        if tls_url && lower_text.starts_with("rediss://") {
            return Some(format!("{place} didn't answer TLS. If the server doesn't use TLS, use redis:// instead of rediss://."));
        }
        let port = summary.and_then(|s| s.port).unwrap_or(default_port);
        let mut hints = vec![format!(
            "A firewall may be blocking port {port}. If you open it, allow only your private network or VPN, not the whole internet."
        )];
        if via_ssh {
            hints.push(ssh_host);
        } else {
            hints.push("Or connect through SSH (Edit Connection > SSH) via a machine that reaches the database.".into());
        }
        return Some(compose(format!("{place}{tunnel} didn't answer in time."), hints));
    }
    if has(&[
        "password authentication failed", "autenticação do tipo senha", "access denied for user", "wrongpass",
        "authentication failed", "invalid username-password", "invalid password",
    ]) {
        return Some(if user.is_empty() {
            "The server refused the login: wrong password, or a user that can't log in.".into()
        } else {
            format!("The server refused the login for \"{user}\": wrong password, or a user that can't log in.")
        });
    }
    if has(&["noauth"]) {
        return Some("The server asks for a password, and the URL has none.".into());
    }
    if has(&["no pg_hba.conf entry", "nenhuma entrada no pg_hba.conf"]) {
        return Some("The server doesn't accept connections from this machine for this user and database (pg_hba.conf).".into());
    }
    if has(&["role \"", "função \"", "papel \""]) && has(&["does not exist", "não existe"]) {
        return Some(format!("The user \"{user}\" doesn't exist on this server."));
    }
    if has(&["database \"", "banco de dados \"", "unknown database"]) && has(&["does not exist", "não existe", "unknown database"]) {
        return Some(format!("The database \"{database}\" doesn't exist on this server."));
    }
    if has(&["db index is out of range"]) {
        return Some(format!("This Redis server has no database {database}; it has fewer numbered databases."));
    }
    if has(&["file is not a database"]) {
        return Some("This file isn't a SQLite database.".into());
    }
    None
}

/// The server's message on one to three lines: libpq-style repeats for each
/// address tried (::1, then 127.0.0.1) and blank lines go.
pub fn clean(raw: &str) -> String {
    static PARENS: OnceLock<Regex> = OnceLock::new();
    let parens = PARENS.get_or_init(|| Regex::new(r"\([^)]*\)").expect("regex"));
    let mut lines: Vec<String> = Vec::new();
    for line in raw.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let core = parens.replace_all(line, "");
        if lines.iter().any(|l| parens.replace_all(l, "") == core) {
            continue;
        }
        lines.push(line.to_string());
    }
    lines.into_iter().take(3).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_refused_connections() {
        let error = explain(&DbError::msg("Connection refused (os error 111)"), "postgres://ana@localhost:5433/loja", false);
        let text = error.to_string();
        assert!(text.starts_with("Nothing answered at localhost:5433"), "{text}");
        assert!(text.contains("usual port is 5432"), "{text}");
        assert!(text.ends_with("Connection refused (os error 111)"));
    }

    #[test]
    fn names_container_hosts() {
        let text = explain(&DbError::msg("failed to lookup address information: Name or service not known"), "mysql://u:p@db/shop", false).to_string();
        assert!(text.contains("container or service name"), "{text}");
    }

    #[test]
    fn names_bad_logins() {
        let text = headline("password authentication failed for user \"ana\"", crate::connector::summary("postgres://ana@db.x.com/loja").as_ref(), "", false);
        assert_eq!(text.as_deref(), Some("The server refused the login for \"ana\": wrong password, or a user that can't log in."));
    }

    #[test]
    fn cleans_repeats() {
        let raw = "connection to server at \"localhost\" (::1), port 1 failed: refused\nconnection to server at \"localhost\" (127.0.0.1), port 1 failed: refused\n\n";
        assert_eq!(clean(raw).lines().count(), 1);
    }
}
