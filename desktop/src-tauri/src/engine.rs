//! The kinds of database dbLens knows, told apart by what was pasted, so
//! nobody has to pick one before connecting. Port of `Engine.swift`.

use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::url::{clean, is_windows_absolute, ConnectionUrl, SqliteLocation};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    Postgres,
    Mysql,
    Sqlite,
    Redis,
    Mongodb,
}

impl Engine {
    pub const ALL: [Engine; 5] = [Engine::Postgres, Engine::Mysql, Engine::Sqlite, Engine::Redis, Engine::Mongodb];

    /// The name people know it by: "PostgreSQL".
    pub fn name(self) -> &'static str {
        match self {
            Engine::Postgres => "PostgreSQL",
            Engine::Mysql => "MySQL",
            Engine::Sqlite => "SQLite",
            Engine::Redis => "Redis",
            Engine::Mongodb => "MongoDB",
        }
    }

    /// The URL scheme the form writes.
    pub fn scheme(self) -> &'static str {
        match self {
            Engine::Postgres => "postgresql",
            Engine::Mysql => "mysql",
            Engine::Sqlite => "sqlite",
            Engine::Redis => "redis",
            Engine::Mongodb => "mongodb",
        }
    }

    /// None for SQLite, which is a file.
    pub fn default_port(self) -> Option<u16> {
        match self {
            Engine::Postgres => Some(5432),
            Engine::Mysql => Some(3306),
            Engine::Sqlite => None,
            Engine::Redis => Some(6379),
            Engine::Mongodb => Some(27017),
        }
    }

    /// The engine a pasted connection string is for, or None when it isn't one.
    ///   postgres://  postgresql://  jdbc:postgresql://  postgresql+driver://  host=… dbname=…
    ///   mysql://  mariadb://  mysqlx://  jdbc:mysql://  jdbc:mariadb://  mysql+driver://
    ///   sqlite:…  file:…  a path ending in .db, .sqlite, .sqlite3, .db3
    ///   redis://  rediss://  redis+sentinel://  (valkey:// too)
    ///   mongodb://  mongodb+srv://
    pub fn detect(text: &str) -> Option<Engine> {
        static RULES: OnceLock<Vec<(Regex, Engine)>> = OnceLock::new();
        static FILE: OnceLock<Regex> = OnceLock::new();
        let value = clean(text);
        if value.is_empty() {
            return None;
        }
        let lower = value.to_lowercase();
        let rules = RULES.get_or_init(|| {
            [
                (r"^(jdbc:)?postgres(ql)?(\+[a-z0-9_]+)?://", Engine::Postgres),
                (r"^(jdbc:)?(mysql|mariadb|mysqlx)(\+[a-z0-9_]+)?://", Engine::Mysql),
                (r"^(sqlite3?|file):", Engine::Sqlite),
                (r"^(rediss?|valkeys?)(\+[a-z]+)?://", Engine::Redis),
                (r"^mongodb(\+srv)?://", Engine::Mongodb),
            ]
            .into_iter()
            .map(|(pattern, engine)| (Regex::new(pattern).expect("engine rule"), engine))
            .collect()
        });
        if let Some((_, engine)) = rules.iter().find(|(re, _)| re.is_match(&lower)) {
            return Some(*engine);
        }
        let file = FILE.get_or_init(|| Regex::new(r"\.(db|db3|sqlite|sqlite3)$").expect("file rule"));
        if file.is_match(&lower)
            && (lower.starts_with('/') || lower.starts_with('~') || lower.starts_with('.') || is_windows_absolute(&lower))
        {
            return Some(Engine::Sqlite);
        }
        // A SQLite path in an .env line (DB_PATH=/x/app.db).
        if SqliteLocation::new(text).is_ok() {
            return Some(Engine::Sqlite);
        }
        // libpq's key=value form: host=… dbname=… user=…
        if ConnectionUrl::new(&value).is_ok() {
            return Some(Engine::Postgres);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_engines() {
        let cases = [
            ("postgres://u:p@db/x", Engine::Postgres),
            ("jdbc:postgresql://db:5432/x?user=u", Engine::Postgres),
            ("DATABASE_URL=\"postgresql+psycopg2://u@db/x\"", Engine::Postgres),
            ("host=db dbname=x user=u", Engine::Postgres),
            ("mysql://root:pw@127.0.0.1:3306/shop", Engine::Mysql),
            ("mariadb://u@db/x", Engine::Mysql),
            ("jdbc:mysql://db:3306/x", Engine::Mysql),
            ("mysql+pymysql://u:p@db/x", Engine::Mysql),
            ("sqlite:///Users/me/app.db", Engine::Sqlite),
            ("~/Projects/app/data.sqlite3", Engine::Sqlite),
            ("file:/tmp/test.db", Engine::Sqlite),
            ("C:\\Users\\me\\app.db", Engine::Sqlite),
            ("DB_PATH=/x/app.db", Engine::Sqlite),
            ("redis://:secret@cache:6379/0", Engine::Redis),
            ("rediss://default:pw@redis.example.com:6380", Engine::Redis),
            ("REDIS_URL=redis://localhost", Engine::Redis),
            ("valkey://localhost:6379", Engine::Redis),
            ("mongodb://u:p@mongo:27017/app?authSource=admin", Engine::Mongodb),
            ("mongodb+srv://u:p@cluster0.abcd.mongodb.net/app", Engine::Mongodb),
        ];
        for (text, engine) in cases {
            assert_eq!(Engine::detect(text), Some(engine), "{text}");
        }
    }

    #[test]
    fn detects_nothing_for_other_text() {
        for text in ["", "hello", "https://example.com", "oracle://db/x"] {
            assert_eq!(Engine::detect(text), None, "{text}");
        }
    }

    #[test]
    fn serializes_lowercase() {
        assert_eq!(serde_json::to_string(&Engine::Mongodb).unwrap(), "\"mongodb\"");
        assert_eq!(Engine::Postgres.default_port(), Some(5432));
        assert_eq!(Engine::Sqlite.default_port(), None);
    }
}
