//! Connects to whatever database a pasted string points at: the engine is read
//! from the text, never chosen by hand. Port of `Connector.swift`.

use std::sync::Arc;

use serde::Serialize;

use crate::connection_problem;
use crate::drivers;
use crate::engine::Engine;
use crate::pool::{Opener, Policy, PooledSession};
use crate::session::{DatabaseSession, DbError, DbResult};
use crate::ssh::{SshConfig, SshTunnel};

/// What the connection sheet shows under the URL, and the default name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub engine: Engine,
    /// The database name, "db 3" for Redis, the file path for SQLite.
    pub database: String,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub user: Option<String>,
    /// The TLS mode when the URL sets one ("require", "REQUIRED", "TLS").
    pub tls: Option<String>,
    pub suggested_name: String,
}

/// The text as it will be saved: without an .env name or quotes around it.
pub fn clean(text: &str) -> String {
    crate::url::clean(text)
}

/// The summary for any engine, or None when the text isn't a connection.
pub fn summary(text: &str) -> Option<Summary> {
    match Engine::detect(text)? {
        Engine::Postgres => drivers::postgres::summary(text),
        Engine::Mysql => drivers::mysql::summary(text),
        Engine::Sqlite => drivers::sqlite::summary(text),
        Engine::Redis => drivers::redis::summary(text),
        Engine::Mongodb => drivers::mongo::summary(text),
    }
}

/// An open session (a small pool, see `pool.rs`) and the tunnel it goes
/// through. The session drops first.
pub struct Connection {
    pub session: Arc<PooledSession>,
    pub tunnel: Option<SshTunnel>,
}

/// Opens a session. A failure names its cause first (connection_problem),
/// then the server's own message.
pub async fn connect(text: &str, ssh: Option<&SshConfig>) -> DbResult<Connection> {
    open(text, ssh).await.map_err(|error| match error {
        DbError::Message(_) => connection_problem::explain(&error, text, ssh.is_some()),
        other => other,
    })
}

async fn open(text: &str, ssh: Option<&SshConfig>) -> DbResult<Connection> {
    let engine = Engine::detect(text)
        .ok_or_else(|| DbError::msg("This doesn't look like a database URL."))?;
    let url = clean(text);
    let ssh = ssh.filter(|c| !c.host.trim().is_empty());
    let tunnel = match ssh {
        None => None,
        Some(_) if engine == Engine::Sqlite => {
            return Err(DbError::msg(
                "A SQLite file is on this computer; SSH doesn't apply.",
            ));
        }
        Some(config) => {
            let summary = summary(&url)
                .ok_or_else(|| DbError::msg("This doesn't look like a database URL."))?;
            let host = summary.host.clone().unwrap_or_else(|| "localhost".into());
            let port = summary.port.or(engine.default_port()).unwrap_or(0);
            Some(SshTunnel::open(config, &host, port).await?)
        }
    };
    let local_port = tunnel.as_ref().map(|t| t.local_port);
    let session = open_driver(engine, &url, local_port).await?;
    let pool = pooled(Arc::from(session), url, local_port);
    pool.warm_up();
    Ok(Connection {
        session: Arc::new(pool),
        tunnel,
    })
}

async fn open_driver(
    engine: Engine,
    url: &str,
    local_port: Option<u16>,
) -> DbResult<Box<dyn DatabaseSession>> {
    match engine {
        Engine::Postgres => drivers::postgres::connect(url, local_port).await,
        Engine::Mysql => drivers::mysql::connect(url, local_port).await,
        Engine::Sqlite => drivers::sqlite::connect(url, local_port).await,
        Engine::Redis => drivers::redis::connect(url, local_port).await,
        Engine::Mongodb => drivers::mongo::connect(url, local_port).await,
    }
}

/// Wraps an open driver session in its engine's pool: more connections to the
/// same `url` (cleaned) and tunnel port, opened when browsing needs them.
/// SQLite's extra connections are read-only.
pub fn pooled(
    pinned: Arc<dyn DatabaseSession>,
    url: String,
    local_port: Option<u16>,
) -> PooledSession {
    let engine = pinned.engine();
    let policy = Policy::for_engine(engine);
    let open: Opener = Arc::new(move || {
        let url = url.clone();
        Box::pin(async move {
            match engine {
                Engine::Sqlite => drivers::sqlite::connect_reader(&url).await,
                other => open_driver(other, &url, local_port).await,
            }
        })
    });
    PooledSession::new(pinned, policy, Some(open))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarizes_every_engine() {
        let pg = summary("postgres://ana:pw@db.x.com:6543/loja?sslmode=require").unwrap();
        assert_eq!(
            (pg.engine, pg.database.as_str(), pg.port, pg.tls.as_deref()),
            (Engine::Postgres, "loja", Some(6543), Some("require"))
        );
        assert_eq!(pg.suggested_name, "loja @ db.x.com");
        let my = summary("mysql://root:pw@127.0.0.1:3306/shop").unwrap();
        assert_eq!(
            (my.engine, my.database.as_str(), my.user.as_deref()),
            (Engine::Mysql, "shop", Some("root"))
        );
        let redis = summary("rediss://default:pw@cache.x.com:6380/2").unwrap();
        assert_eq!(
            (redis.database.as_str(), redis.tls.as_deref()),
            ("db 2", Some("TLS"))
        );
        let mongo = summary("mongodb+srv://u:p@cluster0.x.net/app").unwrap();
        assert_eq!(
            (mongo.engine, mongo.database.as_str()),
            (Engine::Mongodb, "app")
        );
        let lite = summary("sqlite:///tmp/app.db").unwrap();
        assert_eq!(
            (lite.engine, lite.suggested_name.as_str()),
            (Engine::Sqlite, "app.db")
        );
        assert!(summary("hello").is_none());
    }

    #[tokio::test]
    async fn refuses_text_that_is_no_url() {
        let error = connect("hello", None).await.err().unwrap();
        assert_eq!(error.to_string(), "This doesn't look like a database URL.");
        let ssh = SshConfig {
            host: "bastion".into(),
            ..Default::default()
        };
        assert!(connect("sqlite:///tmp/x.db", Some(&ssh)).await.is_err());
    }
}
