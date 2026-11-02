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
/// $tag$ bodies and E'…' strings (Postgres), or comments. Postgres block
/// comments nest; in MySQL `--` starts a comment only before a space or a
/// control character, and `#` starts one too. Empty statements are dropped.
pub fn statements(sql: &str, dialect: Dialect) -> Vec<String> {
    let chars: Vec<char> = sql.chars().collect();
    let len = chars.len();
    let mut out: Vec<String> = Vec::new();
    let mut start = 0;
    let mut i = 0;
    // Moves past a quoted text that started before `i`, ending at `quote`.
    // A doubled quote reads as two texts in a row, which comes to the same.
    let skip_quoted = |i: &mut usize, quote: char, backslashes: bool| {
        while *i < len && chars[*i] != quote {
            if backslashes && chars[*i] == '\\' {
                *i += 1;
            }
            *i += 1;
        }
        *i = len.min(*i + 1);
    };
    let word_char = |c: char| c.is_alphanumeric() || c == '_';
    while i < len {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        match c {
            '\'' => {
                // Postgres E'…' strings take backslash escapes (E not ending a word).
                let escape_string = dialect == Dialect::Postgres
                    && i > 0
                    && matches!(chars[i - 1], 'e' | 'E')
                    && (i < 2 || !word_char(chars[i - 2]));
                i += 1;
                skip_quoted(&mut i, '\'', dialect == Dialect::Mysql || escape_string);
            }
            '"' => {
                i += 1;
                skip_quoted(&mut i, '"', dialect == Dialect::Mysql);
            }
            '`' if dialect != Dialect::Postgres => {
                i += 1;
                skip_quoted(&mut i, '`', false);
            }
            '[' if dialect == Dialect::Sqlite => {
                i += 1;
                skip_quoted(&mut i, ']', false);
            }
            '-' if next == Some('-')
                && (dialect != Dialect::Mysql
                    || !matches!(chars.get(i + 2), Some(c) if !c.is_whitespace() && !c.is_control())) =>
            {
                while i < len && chars[i] != '\n' {
                    i += 1;
                }
            }
            '#' if dialect == Dialect::Mysql => {
                while i < len && chars[i] != '\n' {
                    i += 1;
                }
            }
            '/' if next == Some('*') => {
                i += 2;
                let mut depth = 1;
                while i < len && depth > 0 {
                    if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                        depth -= 1;
                        i += 2;
                    } else if dialect == Dialect::Postgres
                        && chars[i] == '/'
                        && chars.get(i + 1) == Some(&'*')
                    {
                        depth += 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                i = i.min(len);
            }
            '$' if dialect == Dialect::Postgres => {
                // $$ or $tag$ … $tag$ (a tag is letters, digits, underscores).
                let mut end = i + 1;
                while end < len && word_char(chars[end]) {
                    end += 1;
                }
                let after_word = i > 0 && word_char(chars[i - 1]);
                let tag_ok = end < len
                    && chars[end] == '$'
                    && !chars.get(i + 1).is_some_and(|c| c.is_ascii_digit());
                if tag_ok && !after_word {
                    let tag: Vec<char> = chars[i..=end].to_vec();
                    i = end + 1;
                    while i < len && !chars[i..].starts_with(&tag) {
                        i += 1;
                    }
                    i = len.min(i + tag.len());
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
    out.push(chars[start.min(len)..i.min(len)].iter().collect());
    out.into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !verb(s, 1).is_empty())
        .collect()
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
            let len = rest
                .find(|c: char| !(c.is_alphabetic() || c == '_'))
                .unwrap_or(rest.len());
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
const TRANSACTION_VERBS: [&str; 8] = [
    "BEGIN",
    "START",
    "COMMIT",
    "END",
    "ROLLBACK",
    "ABORT",
    "SAVEPOINT",
    "RELEASE",
];

/// The statements, or an error naming the first one that controls the transaction.
pub fn checked(sql: &str, dialect: Dialect) -> DbResult<Vec<String>> {
    let list = statements(sql, dialect);
    if list.is_empty() {
        return Err(DbError::msg("There is no statement to run."));
    }
    for (index, statement) in list.iter().enumerate() {
        let first = verb(statement, 1);
        let two = verb(statement, 2);
        if TRANSACTION_VERBS.contains(&first.as_str())
            || two == "PREPARE TRANSACTION"
            || two == "SET AUTOCOMMIT"
        {
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
    DbError::msg(format!(
        "Statement {} of {count} failed ({shown}): {error}",
        index + 1
    ))
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
        assert_eq!(
            statements(sql, Dialect::Postgres),
            vec!["select 'a;b'", "-- c;d\nselect \"x;y\" /* ; */", "select 2"]
        );
    }

    #[test]
    fn keeps_dollar_bodies_whole() {
        let sql =
            "create function f() returns int as $$ select 1; $$ language sql; select $tag$;$tag$";
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
    fn postgres_escape_strings_and_nested_comments() {
        let list = statements(r"select E'it\'s; x'; select 2", Dialect::Postgres);
        assert_eq!(list, vec![r"select E'it\'s; x'", "select 2"]);
        // A plain string has no backslash escapes.
        let list = statements(r"select 'a\'; select 2", Dialect::Postgres);
        assert_eq!(list, vec![r"select 'a\'", "select 2"]);
        // "e" ending a word is not an escape-string prefix.
        let list = statements(r"select name'x\'; select 2", Dialect::Postgres);
        assert_eq!(list.len(), 2);
        let list = statements("select 1 /* /* */ ; */; select 2", Dialect::Postgres);
        assert_eq!(list, vec!["select 1 /* /* */ ; */", "select 2"]);
        let list = statements("select $1; select 2", Dialect::Postgres);
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn mysql_dash_comments_need_a_space() {
        let list = statements("select 1--1; drop table t", Dialect::Mysql);
        assert_eq!(list, vec!["select 1--1", "drop table t"]);
        let list = statements("select 1 -- note; x\nselect 2", Dialect::Mysql);
        assert_eq!(list, vec!["select 1 -- note; x\nselect 2"]);
        // MySQL block comments do not nest.
        let list = statements("select 1 /* /* */ ; select 2", Dialect::Mysql);
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn sqlite_bracket_identifiers() {
        let list = statements("select [a;b] from t; select 2", Dialect::Sqlite);
        assert_eq!(list, vec!["select [a;b] from t", "select 2"]);
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
        assert_eq!(
            checked("insert into t values (1); select 1", Dialect::Sqlite)
                .unwrap()
                .len(),
            2
        );
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
        assert_eq!(
            error.to_string(),
            "Statement 2 of 2 failed (boom): syntax error"
        );
    }
}
