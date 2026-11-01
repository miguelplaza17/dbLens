//! A SQL script as its statements, for running them one at a time inside a
//! transaction dbLens controls (rehearsals and all-or-nothing runs). Port of
//! `SQLScript.swift`; drivers use it for `rehearse` and `execute_atomically`.

use crate::session::{DbError, DbResult, QueryResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Postgres,
    Mysql,
    Sqlite,
}

/// The script cut at semicolons that are not inside quotes, `identifiers`,
/// $tag$ bodies (Postgres), or comments. Empty statements are dropped.
pub fn statements(sql: &str, dialect: Dialect) -> Vec<String> {
    let chars: Vec<char> = sql.chars().collect();
    let mut out: Vec<String> = Vec::new();
    let mut start = 0;
    let mut i = 0;
    let skip_past = |i: &mut usize, terminator: &[char]| {
        while *i < chars.len() && !chars[*i..].starts_with(terminator) {
            // A backslash escapes the next character in MySQL strings.
            if dialect == Dialect::Mysql && chars[*i] == '\\' && terminator.len() == 1 && terminator != ['\n'] {
                *i += 1;
            }
            *i += 1;
        }
        *i = chars.len().min(*i + terminator.len());
    };
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        match c {
            '\'' | '"' => {
                i += 1;
                skip_past(&mut i, &[c]);
            }
            '`' if dialect != Dialect::Postgres => {
                i += 1;
                skip_past(&mut i, &['`']);
            }
            '-' if next == Some('-') => skip_past(&mut i, &['\n']),
            '#' if dialect == Dialect::Mysql => skip_past(&mut i, &['\n']),
            '/' if next == Some('*') => {
                i += 2;
                skip_past(&mut i, &['*', '/']);
            }
            '$' if dialect == Dialect::Postgres => {
                // $$ or $tag$ … $tag$ (a tag is letters, digits, underscores).
                let mut end = i + 1;
                while end < chars.len() && (chars[end].is_alphanumeric() || chars[end] == '_') {
                    end += 1;
                }
                let after_word = i > 0 && chars[i - 1].is_alphanumeric();
                if end < chars.len() && chars[end] == '$' && !after_word {
                    let tag: Vec<char> = chars[i..=end].to_vec();
                    i = end + 1;
                    skip_past(&mut i, &tag);
                } else {
                    i += 1;
                }
            }
            ';' => {
                out.push(chars[start..i].iter().collect());
                i += 1;
                start = i;
            }
            _ => i += 1,
        }
    }
    out.push(chars[start.min(chars.len())..i.min(chars.len())].iter().collect());
    out.into_iter().map(|s| s.trim().to_string()).filter(|s| !verb(s, 1).is_empty()).collect()
}

/// The statement's first word(s), uppercased, after comments: "SELECT",
/// "CREATE TEMPORARY", "START TRANSACTION". Empty for a comment alone.
pub fn verb(statement: &str, words: usize) -> String {
    let mut rest = statement;
    let mut found: Vec<String> = Vec::new();
    while found.len() < words {
        rest = rest.trim_start_matches(|c: char| c.is_whitespace() || c == '(');
        if rest.starts_with("--") || rest.starts_with('#') {
            rest = rest.find('\n').map(|n| &rest[n..]).unwrap_or("");
        } else if rest.starts_with("/*") {
            match rest.find("*/") {
                Some(end) => rest = &rest[end + 2..],
                None => break,
            }
        } else {
            let len = rest.find(|c: char| !(c.is_alphabetic() || c == '_')).unwrap_or(rest.len());
            if len == 0 {
                break;
            }
            found.push(rest[..len].to_uppercase());
            rest = &rest[len..];
        }
    }
    found.join(" ")
}

/// Statements that end or open a transaction: refused in scripts whose
/// transaction dbLens owns, since a COMMIT inside would make a test permanent.
const TRANSACTION_VERBS: [&str; 8] = ["BEGIN", "START", "COMMIT", "END", "ROLLBACK", "ABORT", "SAVEPOINT", "RELEASE"];

/// The statements, or an error naming the first one that controls the transaction.
pub fn checked(sql: &str, dialect: Dialect) -> DbResult<Vec<String>> {
    let list = statements(sql, dialect);
    if list.is_empty() {
        return Err(DbError::msg("There is no statement to run."));
    }
    for (index, statement) in list.iter().enumerate() {
        let first = verb(statement, 1);
        let two = verb(statement, 2);
        if TRANSACTION_VERBS.contains(&first.as_str()) || two == "PREPARE TRANSACTION" || two == "SET AUTOCOMMIT" {
            return Err(DbError::msg(format!(
                "Statement {} ({first}) controls the transaction; dbLens runs the script in its own. Leave out BEGIN, COMMIT, and ROLLBACK.",
                index + 1
            )));
        }
    }
    Ok(list)
}

/// The error for statement `index` (0-based) of `count`, naming it.
pub fn failed(index: usize, count: usize, statement: &str, error: &DbError) -> DbError {
    let shown: String = if statement.chars().count() > 80 {
        statement.chars().take(80).collect::<String>() + "…"
    } else {
        statement.to_string()
    };
    DbError::msg(format!("Statement {} of {count} failed ({shown}): {error}", index + 1))
}

/// Runs each statement in order with an async body, naming the one that fails.
pub async fn run<F, Fut>(statements: &[String], mut body: F) -> DbResult<Vec<QueryResult>>
where
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = DbResult<QueryResult>>,
{
    let mut results = Vec::with_capacity(statements.len());
    for (index, statement) in statements.iter().enumerate() {
        match body(statement.clone()).await {
            Ok(result) => results.push(result),
            Err(DbError::Cancelled) => return Err(DbError::Cancelled),
            Err(error) => return Err(failed(index, statements.len(), statement, &error)),
        }
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_outside_quotes_and_comments() {
        let sql = "select 'a;b'; -- c;d\nselect \"x;y\" /* ; */ ; select 2;;";
        assert_eq!(statements(sql, Dialect::Postgres), vec!["select 'a;b'", "-- c;d\nselect \"x;y\" /* ; */", "select 2"]);
    }

    #[test]
    fn keeps_dollar_bodies_whole() {
        let sql = "create function f() returns int as $$ select 1; $$ language sql; select $tag$;$tag$";
        let list = statements(sql, Dialect::Postgres);
        assert_eq!(list.len(), 2);
        assert!(list[0].ends_with("language sql"));
    }

    #[test]
    fn mysql_backslashes_and_hashes() {
        let list = statements("select 'it\\'s; ok'; # note;\nselect `a;b`", Dialect::Mysql);
        assert_eq!(list, vec!["select 'it\\'s; ok'", "# note;\nselect `a;b`"]);
    }

    #[test]
    fn verbs_skip_comments() {
        assert_eq!(verb("/* x */ -- y\n  (select 1)", 1), "SELECT");
        assert_eq!(verb("create temporary table t()", 2), "CREATE TEMPORARY");
        assert_eq!(verb("-- only a comment", 1), "");
    }

    #[test]
    fn refuses_transaction_control() {
        assert!(checked("insert into t values (1); commit", Dialect::Postgres).is_err());
        assert!(checked("set autocommit = 0", Dialect::Mysql).is_err());
        assert!(checked("  -- nothing\n", Dialect::Sqlite).is_err());
        assert_eq!(checked("insert into t values (1); select 1", Dialect::Sqlite).unwrap().len(), 2);
    }

    #[tokio::test]
    async fn run_names_the_failing_statement() {
        let list = vec!["select 1".to_string(), "boom".to_string()];
        let error = run(&list, |s| async move {
            if s == "boom" {
                Err(DbError::msg("syntax error"))
            } else {
                Ok(QueryResult::command("SELECT 1", 1))
            }
        })
        .await
        .unwrap_err();
        assert_eq!(error.to_string(), "Statement 2 of 2 failed (boom): syntax error");
    }
}
