//! One module per engine. Each exports:
//!
//! ```ignore
//! pub fn summary(text: &str) -> Option<Summary>;
//! pub async fn connect(url: &str, local_port: Option<u16>) -> DbResult<Box<dyn DatabaseSession>>;
//! ```
//!
//! `url` is already cleaned (`url::clean`). `local_port` is Some when the
//! connection goes through an SSH tunnel: connect to 127.0.0.1:<local_port>
//! instead of the URL's host and port (keep the URL's host for TLS names).

pub mod mongo;
pub mod mongo_shell;
pub mod mysql;
pub mod postgres;
pub mod redis;
pub mod sqlite;
