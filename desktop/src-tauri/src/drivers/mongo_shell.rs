//! mongosh-style text, as typed in the command panel: how it splits into
//! statements, what each statement asks for, and the object literals its
//! arguments are written in, turned into the extended JSON the bson crate reads.
//! Port of `Sources/DBLensKit/MongoShell.swift`.

use std::sync::OnceLock;

use base64::Engine as _;
use regex::Regex;

use crate::session::{DbError, DbResult};

/// What one statement asks for. Arguments stay as typed, to be read with `extended_json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Statement {
    /// show dbs, show databases
    ShowDatabases,
    /// show collections, show tables
    ShowCollections,
    /// use shop
    Use(String),
    /// db.runCommand({…}), db.getCollectionNames(), …
    Database { method: String, arguments: Vec<String> },
    /// db.users.find({…}).sort({…}).limit(5), db.getCollection("x").drop(), …
    Collection { name: String, calls: Vec<Call> },
}

/// One method call of a chain: `find({a: 1})` is ("find", ["{a: 1}"]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    pub name: String,
    pub arguments: Vec<String>,
}

impl Call {
    pub fn new(name: &str, arguments: &[&str]) -> Self {
        Self { name: name.into(), arguments: arguments.iter().map(|a| a.to_string()).collect() }
    }
}

/// The error for a statement dbLens doesn't know.
pub fn unsupported(statement: &str) -> DbError {
    let line = statement.split('\n').find(|l| !l.is_empty()).unwrap_or(statement);
    DbError::msg(format!("dbLens can't run this yet: {}", line.trim_matches(|c| c == ' ' || c == '\t')))
}

// MARK: Statements

/// Splits the panel's text into statements: at `;` and at line ends, but not
/// while a bracket is open or when the next line goes on with `.method()`.
/// Comments (`//`, `/* */`) are dropped; strings and regexes are kept as typed.
pub fn statements(text: &str) -> Vec<String> {
    let s: Vec<char> = text.chars().collect();
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut depth: i32 = 0;
    let mut last: Option<char> = None;
    let mut i = 0;
    fn flush(out: &mut Vec<String>, current: &mut String, last: &mut Option<char>, depth: &mut i32) {
        let statement = current.trim();
        if !statement.is_empty() {
            out.push(statement.to_string());
        }
        current.clear();
        *last = None;
        *depth = 0;
    }
    while i < s.len() {
        let c = s[i];
        if let Some(end) = comment_end(&s, i) {
            current.push(' ');
            i = end;
            continue;
        }
        if matches!(c, '"' | '\'' | '`') {
            let end = string_end(&s, i);
            current.extend(&s[i..end]);
            last = Some(c);
            i = end;
            continue;
        }
        if c == '/' && regex_may_start(last) {
            let end = regex_end(&s, i);
            current.extend(&s[i..end]);
            last = Some('/');
            i = end;
            continue;
        }
        if "([{".contains(c) {
            depth += 1;
        }
        if ")]}".contains(c) {
            depth = (depth - 1).max(0);
        }
        if depth == 0 && c == ';' {
            flush(&mut out, &mut current, &mut last, &mut depth);
            i += 1;
            continue;
        }
        if depth == 0 && c == '\n' {
            if last == Some('.') || next_is_chained_call(&s, i + 1) {
                current.push(' ');
            } else {
                flush(&mut out, &mut current, &mut last, &mut depth);
            }
            i += 1;
            continue;
        }
        current.push(c);
        if !c.is_whitespace() {
            last = Some(c);
        }
        i += 1;
    }
    flush(&mut out, &mut current, &mut last, &mut depth);
    out
}

/// What one statement (as `statements` returns it) asks for, or the
/// "can't run this yet" error.
pub fn parse(statement: &str) -> DbResult<Statement> {
    let trimmed = statement.trim_matches(|c: char| c.is_whitespace() || c == ';');
    let words: Vec<&str> = trimmed.split_whitespace().collect();
    if words.first() == Some(&"show") && words.len() == 2 {
        return match words[1].to_lowercase().as_str() {
            "dbs" | "databases" => Ok(Statement::ShowDatabases),
            "collections" | "tables" => Ok(Statement::ShowCollections),
            _ => Err(unsupported(statement)),
        };
    }
    if words.first() == Some(&"use") && words.len() == 2 {
        let name = words[1].trim_matches(|c| c == '"' || c == '\'' || c == '`');
        if name.is_empty() {
            return Err(unsupported(statement));
        }
        return Ok(Statement::Use(name.to_string()));
    }
    let mut reader = Lexer::new(trimmed);
    parse_chain(&mut reader).map_err(|_| unsupported(statement))
}

/// db, then a collection (`.name`, `["name"]`, `.getCollection("name")`) and its
/// method calls, or a database method call.
fn parse_chain(reader: &mut Lexer) -> Result<Statement, Failure> {
    reader.skip_space();
    if reader.identifier().as_deref() != Some("db") {
        return Err(Failure::default());
    }
    let mut parts: Vec<String> = Vec::new();
    let mut collection: Option<String> = None;
    let mut calls: Vec<Call> = Vec::new();
    loop {
        reader.skip_space();
        if reader.at_end() {
            break;
        }
        if collection.is_none() && parts.is_empty() && reader.consume('[') {
            let args = reader.arguments(']')?;
            if args.len() != 1 {
                return Err(Failure::default());
            }
            collection = Some(string_argument(&args[0]).ok_or_else(Failure::default)?);
            continue;
        }
        if !reader.consume('.') {
            return Err(Failure::default());
        }
        reader.skip_space();
        let name = reader.identifier().ok_or_else(Failure::default)?;
        reader.skip_space();
        if reader.peek() != Some('(') {
            if collection.is_some() || !calls.is_empty() {
                return Err(Failure::default());
            }
            parts.push(name);
            continue;
        }
        reader.advance();
        let args = reader.arguments(')')?;
        if collection.is_none() && parts.is_empty() {
            if name == "getCollection" {
                if args.len() != 1 {
                    return Err(Failure::default());
                }
                collection = Some(string_argument(&args[0]).ok_or_else(Failure::default)?);
                continue;
            }
            reader.skip_space();
            if !reader.at_end() {
                return Err(Failure::default());
            }
            return Ok(Statement::Database { method: name, arguments: args });
        }
        if collection.is_none() {
            collection = Some(parts.join("."));
        }
        calls.push(Call { name, arguments: args });
    }
    match collection {
        Some(name) if !name.is_empty() && !calls.is_empty() => Ok(Statement::Collection { name, calls }),
        _ => Err(Failure::default()),
    }
}

/// A string argument ("name" or 'name'), as its text.
pub fn string_argument(argument: &str) -> Option<String> {
    let mut reader = Lexer::new(argument);
    reader.skip_space();
    if !matches!(reader.peek(), Some('"' | '\'' | '`')) {
        return None;
    }
    let text = reader.string().ok()?;
    reader.skip_space();
    reader.at_end().then_some(text)
}

// MARK: Literals

/// A mongosh value literal as extended JSON (the form `Bson::try_from(serde_json::Value)`
/// reads): unquoted keys and 'single quotes' become JSON strings, trailing commas
/// go, and the shell's helpers become their extended JSON: ObjectId("…") →
/// {"$oid": …}, ISODate("…") and new Date("…") → {"$date": …}, NumberLong,
/// NumberInt, NumberDecimal, UUID, Timestamp, BinData, MinKey, MaxKey, and
/// /regex/flags → {"$regularExpression": …}. Plain JSON and extended JSON pass through.
pub fn extended_json(literal: &str) -> DbResult<String> {
    let mut reader = Lexer::new(literal);
    let outcome = reader.value().and_then(|value| {
        reader.skip_space();
        if reader.at_end() {
            Ok(value.json())
        } else {
            Err(Failure::default())
        }
    });
    outcome.map_err(|failure| match failure.message {
        Some(message) => DbError::msg(message),
        None => {
            let snippet = literal.trim();
            let short = if snippet.chars().count() > 80 {
                format!("{}…", snippet.chars().take(80).collect::<String>())
            } else {
                snippet.to_string()
            };
            DbError::msg(format!("This isn't a value dbLens can read: {short}"))
        }
    })
}

/// A JSON string literal: "a \"b\"".
pub fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// 1.5, 1.0, 1e+20, NaN, Infinity, -Infinity (as Swift prints a Double).
pub fn double_text(value: f64) -> String {
    if value.is_nan() {
        return "NaN".into();
    }
    if value.is_infinite() {
        return if value < 0.0 { "-Infinity".into() } else { "Infinity".into() };
    }
    let text = format!("{value:?}");
    match text.find('e') {
        Some(at) => {
            let (mantissa, exponent) = (&text[..at], &text[at + 1..]);
            let (sign, digits) = match exponent.strip_prefix('-') {
                Some(d) => ("-", d),
                None => ("+", exponent),
            };
            let mantissa = mantissa.strip_suffix(".0").unwrap_or(mantissa);
            format!("{mantissa}e{sign}{digits:0>2}")
        }
        None => text,
    }
}

/// A Decimal128 as typed ("9.99", "1E+3").
pub fn is_decimal(text: &str) -> bool {
    !text.is_empty() && text.parse::<mongodb::bson::Decimal128>().is_ok()
}

// MARK: Dates

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("valid regex"))
}

/// Milliseconds since 1970 for an ISO 8601 date, read the way mongosh's ISODate
/// does: "2024-05-01", "2024-05-01T10:30", "2024-05-01T10:30:00.250Z",
/// "2024-05-01 10:30:00+02:00". Without a zone, UTC. None when it isn't one.
pub fn iso_date_milliseconds(text: &str) -> Option<i64> {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    let pattern = re(
        &PATTERN,
        r"^([0-9]{4})-([0-9]{2})-([0-9]{2})(?:[Tt ]([0-9]{2}):([0-9]{2})(?::([0-9]{2})(?:[.,]([0-9]{1,9}))?)?)?\s*(Z|z|[+-][0-9]{2}(?::?[0-9]{2})?)?$",
    );
    let value = text.trim_matches(|c| c == ' ' || c == '\t');
    let m = pattern.captures(value)?;
    let group = |i: usize| m.get(i).map(|g| g.as_str());
    let number = |i: usize| group(i).map_or(Some(0), |g| g.parse::<i64>().ok());
    let (year, month, day) = (group(1)?.parse::<i64>().ok()?, group(2)?.parse::<i64>().ok()?, group(3)?.parse::<i64>().ok()?);
    let (hour, minute, second) = (number(4)?, number(5)?, number(6)?);
    if !(1..=12).contains(&month) || day < 1 || day > days_in(month, year) || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let fraction = format!("{}000", group(7).unwrap_or(""));
    let millis: i64 = fraction[..3].parse().unwrap_or(0);
    let mut offset = 0;
    if let Some(zone) = group(8).filter(|z| !z.eq_ignore_ascii_case("z")) {
        let sign = if zone.starts_with('-') { -1 } else { 1 };
        let digits = zone[1..].replace(':', "");
        let hours: i64 = digits.get(..2).and_then(|d| d.parse().ok()).unwrap_or(0);
        let minutes: i64 = if digits.len() > 2 { digits[2..].parse().unwrap_or(0) } else { 0 };
        if hours > 23 || minutes > 59 {
            return None;
        }
        offset = sign * (hours * 3600 + minutes * 60);
    }
    let seconds = days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second - offset;
    Some(seconds * 1000 + millis)
}

/// "2024-05-01T10:30:00.250Z" for milliseconds since 1970 (UTC), or None
/// outside the years 0–9999.
pub fn iso_date(milliseconds: i64) -> Option<String> {
    let (seconds, millis) = (milliseconds / 1000, milliseconds % 1000);
    let whole_seconds = if millis < 0 { seconds - 1 } else { seconds };
    let ms = if millis < 0 { millis + 1000 } else { millis };
    let mut days = whole_seconds / 86_400;
    let mut rest = whole_seconds % 86_400;
    if rest < 0 {
        rest += 86_400;
        days -= 1;
    }
    let (year, month, day) = civil_from_days(days);
    if !(0..=9999).contains(&year) {
        return None;
    }
    Some(format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{ms:03}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    ))
}

fn days_in(month: i64, year: i64) -> i64 {
    match month {
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let doy = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

pub fn date_json(milliseconds: i64) -> String {
    format!(r#"{{"$date":{{"$numberLong":"{milliseconds}"}}}}"#)
}

// MARK: Lexer

/// Why a literal or statement didn't read; `message` is shown as is (helpers with a bad argument).
#[derive(Debug, Default)]
struct Failure {
    message: Option<String>,
}

impl Failure {
    fn say(message: String) -> Self {
        Self { message: Some(message) }
    }
}

/// A literal read so far: strings and numbers are kept apart so the shell's
/// helpers can take them as arguments.
enum Value {
    Str(String),
    Number(String),
    Json(String),
}

impl Value {
    fn json(&self) -> String {
        match self {
            Value::Str(text) => json_string(text),
            Value::Number(text) | Value::Json(text) => text.clone(),
        }
    }
}

/// Reads mongosh text one Unicode scalar at a time.
struct Lexer {
    s: Vec<char>,
    position: usize,
}

fn is_identifier_start(c: char) -> bool {
    c == '_' || c == '$' || c.is_alphabetic()
}

fn is_identifier_part(c: char) -> bool {
    is_identifier_start(c) || c.is_ascii_digit()
}

impl Lexer {
    fn new(text: &str) -> Self {
        Self { s: text.chars().collect(), position: 0 }
    }

    fn at_end(&self) -> bool {
        self.position >= self.s.len()
    }

    fn peek(&self) -> Option<char> {
        self.s.get(self.position).copied()
    }

    fn advance(&mut self) {
        self.position += 1;
    }

    fn consume(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.position += 1;
            true
        } else {
            false
        }
    }

    fn text(&self, from: usize, to: usize) -> String {
        self.s[from..to].iter().collect()
    }

    fn skip_space(&mut self) {
        while self.position < self.s.len() {
            if self.s[self.position].is_whitespace() {
                self.position += 1;
            } else if let Some(end) = comment_end(&self.s, self.position) {
                self.position = end;
            } else {
                return;
            }
        }
    }

    fn identifier(&mut self) -> Option<String> {
        let first = self.peek()?;
        if !is_identifier_start(first) {
            return None;
        }
        let start = self.position;
        while self.peek().is_some_and(is_identifier_part) {
            self.position += 1;
        }
        Some(self.text(start, self.position))
    }

    /// The arguments up to the closing bracket, split at top-level commas, as typed.
    fn arguments(&mut self, close: char) -> Result<Vec<String>, Failure> {
        let mut args: Vec<String> = Vec::new();
        let mut current = String::new();
        let mut depth: i32 = 0;
        let mut last: Option<char> = Some('(');
        while self.position < self.s.len() {
            let c = self.s[self.position];
            if let Some(end) = comment_end(&self.s, self.position) {
                current.push(' ');
                self.position = end;
                continue;
            }
            if matches!(c, '"' | '\'' | '`') {
                let end = string_end(&self.s, self.position);
                current.extend(&self.s[self.position..end]);
                self.position = end;
                last = Some(c);
                continue;
            }
            if c == '/' && regex_may_start(last) {
                let end = regex_end(&self.s, self.position);
                current.extend(&self.s[self.position..end]);
                self.position = end;
                last = Some('/');
                continue;
            }
            self.position += 1;
            if depth == 0 && c == close {
                let text = current.trim().to_string();
                if !text.is_empty() || !args.is_empty() {
                    args.push(text);
                }
                // A trailing comma: f(a, b,)
                if args.last().is_some_and(String::is_empty) {
                    args.pop();
                }
                return Ok(args);
            }
            if "([{".contains(c) {
                depth += 1;
            }
            if ")]}".contains(c) {
                depth -= 1;
            }
            if depth < 0 {
                return Err(Failure::default());
            }
            if depth == 0 && c == ',' {
                args.push(current.trim().to_string());
                current.clear();
                last = Some(c);
                continue;
            }
            current.push(c);
            if !c.is_whitespace() {
                last = Some(c);
            }
        }
        Err(Failure::default())
    }

    // MARK: Values

    fn value(&mut self) -> Result<Value, Failure> {
        self.skip_space();
        let c = self.peek().ok_or_else(Failure::default)?;
        match c {
            '{' => Ok(Value::Json(self.object()?)),
            '[' => Ok(Value::Json(self.array()?)),
            '"' | '\'' | '`' => Ok(Value::Str(self.string()?)),
            '/' => Ok(Value::Json(self.regex()?)),
            '-' | '+' | '.' | '0'..='9' => self.number(),
            c if is_identifier_start(c) => self.word(),
            _ => Err(Failure::default()),
        }
    }

    fn object(&mut self) -> Result<String, Failure> {
        self.position += 1;
        let mut members: Vec<String> = Vec::new();
        loop {
            self.skip_space();
            if self.consume('}') {
                break;
            }
            let key = self.key()?;
            self.skip_space();
            if !self.consume(':') {
                return Err(Failure::default());
            }
            members.push(format!("{}:{}", json_string(&key), self.value()?.json()));
            self.skip_space();
            if self.consume(',') {
                continue;
            }
            if !self.consume('}') {
                return Err(Failure::default());
            }
            break;
        }
        Ok(format!("{{{}}}", members.join(",")))
    }

    fn key(&mut self) -> Result<String, Failure> {
        let c = self.peek().ok_or_else(Failure::default)?;
        if matches!(c, '"' | '\'' | '`') {
            return self.string();
        }
        let start = self.position;
        // Lenient: a.b and numbers are taken as they are.
        while self.peek().is_some_and(|c| is_identifier_part(c) || c == '.' || c == '-') {
            self.position += 1;
        }
        if self.position == start {
            return Err(Failure::default());
        }
        Ok(self.text(start, self.position))
    }

    fn array(&mut self) -> Result<String, Failure> {
        self.position += 1;
        let mut items: Vec<String> = Vec::new();
        loop {
            self.skip_space();
            if self.consume(']') {
                break;
            }
            items.push(self.value()?.json());
            self.skip_space();
            if self.consume(',') {
                continue;
            }
            if !self.consume(']') {
                return Err(Failure::default());
            }
            break;
        }
        Ok(format!("[{}]", items.join(",")))
    }

    /// A quoted string, with JavaScript's escapes read.
    fn string(&mut self) -> Result<String, Failure> {
        let quote = self.peek().ok_or_else(Failure::default)?;
        self.position += 1;
        let mut out = String::new();
        let mut pending_high: Option<u32> = None;
        fn append(out: &mut String, pending_high: &mut Option<u32>, value: u32) {
            if (0xD800..=0xDBFF).contains(&value) {
                if pending_high.is_some() {
                    out.push('\u{FFFD}');
                }
                *pending_high = Some(value);
                return;
            }
            if let Some(high) = *pending_high {
                *pending_high = None;
                if (0xDC00..=0xDFFF).contains(&value) {
                    out.push(char::from_u32(0x10000 + ((high - 0xD800) << 10) + (value - 0xDC00)).unwrap_or('\u{FFFD}'));
                    return;
                }
                out.push('\u{FFFD}');
            }
            out.push(char::from_u32(value).unwrap_or('\u{FFFD}'));
        }
        while self.position < self.s.len() {
            let c = self.s[self.position];
            self.position += 1;
            if c == quote {
                if pending_high.is_some() {
                    out.push('\u{FFFD}');
                }
                return Ok(out);
            }
            if c != '\\' {
                if c == '\n' && quote != '`' {
                    return Err(Failure::default());
                }
                append(&mut out, &mut pending_high, c as u32);
                continue;
            }
            let Some(&e) = self.s.get(self.position) else { break };
            self.position += 1;
            match e {
                'n' => append(&mut out, &mut pending_high, 0x0A),
                't' => append(&mut out, &mut pending_high, 0x09),
                'r' => append(&mut out, &mut pending_high, 0x0D),
                'b' => append(&mut out, &mut pending_high, 0x08),
                'f' => append(&mut out, &mut pending_high, 0x0C),
                'v' => append(&mut out, &mut pending_high, 0x0B),
                '0' => append(&mut out, &mut pending_high, 0),
                '\n' => continue,
                'x' => {
                    let value = self.hex(2).ok_or_else(Failure::default)?;
                    append(&mut out, &mut pending_high, value);
                }
                'u' => {
                    if self.consume('{') {
                        let start = self.position;
                        while self.peek().is_some_and(|c| c != '}') {
                            self.position += 1;
                        }
                        let digits = self.text(start, self.position);
                        if !self.consume('}') {
                            return Err(Failure::default());
                        }
                        let value = u32::from_str_radix(&digits, 16).map_err(|_| Failure::default())?;
                        append(&mut out, &mut pending_high, value);
                    } else {
                        let value = self.hex(4).ok_or_else(Failure::default)?;
                        append(&mut out, &mut pending_high, value);
                    }
                }
                e => append(&mut out, &mut pending_high, e as u32),
            }
        }
        Err(Failure::default())
    }

    fn hex(&mut self, count: usize) -> Option<u32> {
        if self.position + count > self.s.len() {
            return None;
        }
        let text = self.text(self.position, self.position + count);
        if !text.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let value = u32::from_str_radix(&text, 16).ok()?;
        self.position += count;
        Some(value)
    }

    /// /pattern/flags. MongoDB keeps the flags i, m, s, x, u and l, sorted.
    fn regex(&mut self) -> Result<String, Failure> {
        let end = regex_end(&self.s, self.position);
        let close = (self.position..end).rev().find(|&i| self.s[i] == '/').filter(|&i| i > self.position);
        let close = close.ok_or_else(Failure::default)?;
        let pattern = self.text(self.position + 1, close);
        let flags = self.text(close + 1, end);
        self.position = end;
        Ok(regex_json(&pattern, &flags))
    }

    fn number(&mut self) -> Result<Value, Failure> {
        let mut sign = "";
        if self.consume('-') {
            sign = "-";
        } else {
            self.consume('+');
        }
        self.skip_space();
        if self.peek().is_some_and(is_identifier_start) {
            return match self.identifier().as_deref() {
                Some("Infinity") => Ok(Value::Json(format!(r#"{{"$numberDouble":"{sign}Infinity"}}"#))),
                _ => Err(Failure::default()),
            };
        }
        if self.peek() == Some('0') && matches!(self.s.get(self.position + 1), Some('x' | 'X')) {
            self.position += 2;
            let start = self.position;
            while self.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
                self.position += 1;
            }
            let value = i64::from_str_radix(&self.text(start, self.position), 16).map_err(|_| Failure::default())?;
            return Ok(Value::Number(format!("{sign}{value}")));
        }
        let digits = |lexer: &mut Lexer| {
            let start = lexer.position;
            while lexer.peek().is_some_and(|c| c.is_ascii_digit() || c == '_') {
                lexer.position += 1;
            }
            lexer.text(start, lexer.position).replace('_', "")
        };
        let mut whole = digits(self);
        let mut fraction: Option<String> = None;
        if self.peek() == Some('.') {
            self.position += 1;
            fraction = Some(digits(self));
        }
        let mut exponent = String::new();
        if matches!(self.peek(), Some('e' | 'E')) {
            self.position += 1;
            let mut exp_sign = "";
            if self.consume('-') {
                exp_sign = "-";
            } else {
                self.consume('+');
            }
            let exp_digits = digits(self);
            if exp_digits.is_empty() {
                return Err(Failure::default());
            }
            exponent = format!("e{exp_sign}{exp_digits}");
        }
        if whole.is_empty() && fraction.as_deref().unwrap_or("").is_empty() {
            return Err(Failure::default());
        }
        // JSON has no leading zeros (007) and no bare points (.5, 5.).
        whole = whole.trim_start_matches('0').to_string();
        if whole.is_empty() {
            whole = "0".into();
        }
        let mut text = format!("{sign}{whole}");
        if let Some(fraction) = fraction {
            text.push('.');
            text.push_str(if fraction.is_empty() { "0" } else { &fraction });
        }
        Ok(Value::Number(text + &exponent))
    }

    /// true, null, ObjectId("…"), new Date(), NumberLong(5), …
    fn word(&mut self) -> Result<Value, Failure> {
        let mut name = self.identifier().ok_or_else(Failure::default)?;
        self.skip_space();
        if name == "new" {
            name = self.identifier().ok_or_else(Failure::default)?;
            self.skip_space();
        }
        let mut args: Option<Vec<Value>> = None;
        if self.consume('(') {
            let mut values = Vec::new();
            loop {
                self.skip_space();
                if self.consume(')') {
                    break;
                }
                values.push(self.value()?);
                self.skip_space();
                if self.consume(',') {
                    continue;
                }
                if !self.consume(')') {
                    return Err(Failure::default());
                }
                break;
            }
            args = Some(values);
        }
        helper(&name, args)
    }
}

fn bad(name: &str, args: &[Value]) -> Failure {
    let shown = args.iter().map(Value::json).collect::<Vec<_>>().join(", ");
    Failure::say(format!("This isn't a valid {name}({shown})."))
}

/// An integer from text, also when written as a whole double (5.0, 1e3).
fn exact_integer<T: TryFrom<i64> + std::str::FromStr>(text: &str) -> Option<T> {
    text.parse::<T>().ok().or_else(|| {
        let d = text.parse::<f64>().ok().filter(|d| d.is_finite() && d.fract() == 0.0 && d.abs() < 9.2e18)?;
        T::try_from(d as i64).ok()
    })
}

fn helper(name: &str, args: Option<Vec<Value>>) -> Result<Value, Failure> {
    let bare = args.is_none();
    match (name, bare) {
        ("true" | "false" | "null", true) => return Ok(Value::Json(name.into())),
        ("undefined", true) => return Ok(Value::Json(r#"{"$undefined":true}"#.into())),
        ("Infinity", true) => return Ok(Value::Json(r#"{"$numberDouble":"Infinity"}"#.into())),
        ("NaN", true) => return Ok(Value::Json(r#"{"$numberDouble":"NaN"}"#.into())),
        ("MinKey", _) => return Ok(Value::Json(r#"{"$minKey":1}"#.into())),
        ("MaxKey", _) => return Ok(Value::Json(r#"{"$maxKey":1}"#.into())),
        _ => {}
    }
    let Some(list) = args else {
        return Err(Failure::say(format!(
            "dbLens doesn't know {name} here. Write the value itself, like \"text\", 42 or {{a: 1}}."
        )));
    };
    let text = |index: usize| -> Option<String> {
        match list.get(index)? {
            Value::Str(v) | Value::Number(v) => Some(v.clone()),
            Value::Json(_) => None,
        }
    };
    match name {
        "ObjectId" | "ObjectID" => {
            let Some(hex) = text(0) else {
                if !list.is_empty() {
                    return Err(bad(name, &list));
                }
                return Ok(Value::Json(format!(r#"{{"$oid":"{}"}}"#, mongodb::bson::oid::ObjectId::new().to_hex())));
            };
            if hex.chars().count() != 24 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(bad(name, &list));
            }
            Ok(Value::Json(format!(r#"{{"$oid":"{}"}}"#, hex.to_lowercase())))
        }
        "ISODate" | "Date" => {
            if list.is_empty() {
                return Ok(Value::Json(date_json(chrono::Utc::now().timestamp_millis())));
            }
            if let Value::Number(ms) = &list[0] {
                if let Some(value) = ms.parse::<i64>().ok().or_else(|| ms.parse::<f64>().ok().map(|d| d as i64)) {
                    return Ok(Value::Json(date_json(value)));
                }
            }
            let ms = text(0).and_then(|v| iso_date_milliseconds(&v)).ok_or_else(|| bad(name, &list))?;
            Ok(Value::Json(date_json(ms)))
        }
        "NumberLong" | "Long" => {
            let number = text(0).and_then(|v| exact_integer::<i64>(&v)).ok_or_else(|| bad(name, &list))?;
            Ok(Value::Json(format!(r#"{{"$numberLong":"{number}"}}"#)))
        }
        "NumberInt" | "Int32" => {
            let number = text(0).and_then(|v| exact_integer::<i32>(&v)).ok_or_else(|| bad(name, &list))?;
            Ok(Value::Json(format!(r#"{{"$numberInt":"{number}"}}"#)))
        }
        "NumberDecimal" | "Decimal128" => {
            let value = text(0).filter(|v| is_decimal(v)).ok_or_else(|| bad(name, &list))?;
            Ok(Value::Json(format!(r#"{{"$numberDecimal":"{value}"}}"#)))
        }
        "Double" => {
            let number = text(0).and_then(|v| v.parse::<f64>().ok()).ok_or_else(|| bad(name, &list))?;
            Ok(Value::Json(format!(r#"{{"$numberDouble":"{}"}}"#, double_text(number))))
        }
        "UUID" => {
            let hex = if list.is_empty() { uuid::Uuid::new_v4().to_string() } else { text(0).unwrap_or_default() };
            let digits = hex.replace('-', "");
            if digits.len() != 32 || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(bad(name, &list));
            }
            let bytes: Vec<u8> = (0..16).map(|i| u8::from_str_radix(&digits[i * 2..i * 2 + 2], 16).unwrap_or(0)).collect();
            let base64 = base64::engine::general_purpose::STANDARD.encode(bytes);
            Ok(Value::Json(format!(r#"{{"$binary":{{"base64":"{base64}","subType":"04"}}}}"#)))
        }
        "BinData" => {
            let subtype = text(0).and_then(|v| v.parse::<u8>().ok());
            let base64 = text(1).filter(|b| base64::engine::general_purpose::STANDARD.decode(b).is_ok());
            match (list.len(), subtype, base64) {
                (2, Some(subtype), Some(base64)) => {
                    Ok(Value::Json(format!(r#"{{"$binary":{{"base64":"{base64}","subType":"{subtype:02x}"}}}}"#)))
                }
                _ => Err(bad(name, &list)),
            }
        }
        "Timestamp" => {
            if list.len() == 2 {
                if let (Some(t), Some(i)) = (text(0).and_then(|v| v.parse::<u32>().ok()), text(1).and_then(|v| v.parse::<u32>().ok())) {
                    return Ok(Value::Json(format!(r#"{{"$timestamp":{{"t":{t},"i":{i}}}}}"#)));
                }
            }
            if let [Value::Json(object)] = list.as_slice() {
                if object.starts_with('{') {
                    return Ok(Value::Json(format!(r#"{{"$timestamp":{object}}}"#)));
                }
            }
            Err(bad(name, &list))
        }
        "RegExp" => {
            let pattern = text(0).ok_or_else(|| bad(name, &list))?;
            Ok(Value::Json(regex_json(&pattern, &text(1).unwrap_or_default())))
        }
        _ => Err(Failure::say(format!("dbLens can't read {name}(…) here."))),
    }
}

/// {"$regularExpression": …} with the flags MongoDB keeps, sorted.
fn regex_json(pattern: &str, flags: &str) -> String {
    let mut options: Vec<char> = flags.chars().filter(|c| "imsxul".contains(*c)).collect();
    options.sort_unstable();
    options.dedup();
    let options: String = options.into_iter().collect();
    format!(r#"{{"$regularExpression":{{"pattern":{},"options":"{options}"}}}}"#, json_string(pattern))
}

// MARK: Scanning

/// Where a comment that starts at `index` ends, or None when none starts there.
fn comment_end(s: &[char], index: usize) -> Option<usize> {
    if s[index] != '/' || index + 1 >= s.len() {
        return None;
    }
    match s[index + 1] {
        '/' => {
            let mut end = index + 2;
            while end < s.len() && s[end] != '\n' {
                end += 1;
            }
            Some(end)
        }
        '*' => {
            let mut end = index + 2;
            while end + 1 < s.len() && !(s[end] == '*' && s[end + 1] == '/') {
                end += 1;
            }
            Some((end + 2).min(s.len()))
        }
        _ => None,
    }
}

/// Just past the closing quote of the string that starts at `index`.
fn string_end(s: &[char], index: usize) -> usize {
    let quote = s[index];
    let mut end = index + 1;
    while end < s.len() {
        let c = s[end];
        if c == '\\' {
            end += 2;
            continue;
        }
        if c == quote {
            return end + 1;
        }
        if c == '\n' && quote != '`' {
            return end;
        }
        end += 1;
    }
    s.len()
}

/// Just past the flags of the regex literal that starts at `index`.
fn regex_end(s: &[char], index: usize) -> usize {
    let mut end = index + 1;
    let mut in_class = false;
    while end < s.len() {
        let c = s[end];
        if c == '\\' {
            end += 2;
            continue;
        }
        if c == '\n' {
            return end;
        }
        if c == '[' {
            in_class = true;
        }
        if c == ']' {
            in_class = false;
        }
        if c == '/' && !in_class {
            end += 1;
            while end < s.len() && s[end].is_alphabetic() {
                end += 1;
            }
            return end;
        }
        end += 1;
    }
    s.len()
}

/// A `/` starts a regex after these (or at the start); after a value it divides.
fn regex_may_start(last: Option<char>) -> bool {
    match last {
        None => true,
        Some(c) => "(,=:[!&|?{};+-*%<>~^".contains(c),
    }
}

/// True when the next line (past spaces and comments) starts with `.name`.
fn next_is_chained_call(s: &[char], start: usize) -> bool {
    let mut index = start;
    while index < s.len() {
        if s[index].is_whitespace() {
            index += 1;
            continue;
        }
        if let Some(end) = comment_end(s, index) {
            index = end;
            continue;
        }
        break;
    }
    index < s.len() && s[index] == '.' && index + 1 < s.len() && is_identifier_start(s[index + 1])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collection(name: &str, calls: Vec<Call>) -> Statement {
        Statement::Collection { name: name.into(), calls }
    }

    #[test]
    fn splits_statements() {
        let text = "// comment line\nuse shop\ndb.users.find({name: \"a;b\"});  db.users.countDocuments()\ndb.users.find({\n  age: {$gt: 3},   // trailing comment\n})\n  .sort({age: -1})\n  .limit(2)\ndb.users.find({name: /a\\/b;c/i}) /* block ; comment */\nshow collections";
        assert_eq!(
            statements(text),
            vec![
                "use shop",
                r#"db.users.find({name: "a;b"})"#,
                "db.users.countDocuments()",
                "db.users.find({\n  age: {$gt: 3},    \n})   .sort({age: -1})   .limit(2)",
                r"db.users.find({name: /a\/b;c/i})",
                "show collections",
            ]
        );
        assert!(statements("  \n // nothing\n ;; ").is_empty());
        assert_eq!(statements("db.a.find().\nlimit(1)"), vec!["db.a.find(). limit(1)"]);
        assert_eq!(statements("db.a.find({x: 'it''s'}); db.b.find()").len(), 2);
    }

    #[test]
    fn parses_statements() {
        let c = Call::new;
        assert_eq!(parse("show dbs").unwrap(), Statement::ShowDatabases);
        assert_eq!(parse("show databases;").unwrap(), Statement::ShowDatabases);
        assert_eq!(parse("show collections").unwrap(), Statement::ShowCollections);
        assert_eq!(parse("show TABLES").unwrap(), Statement::ShowCollections);
        assert_eq!(parse("use other").unwrap(), Statement::Use("other".into()));
        assert_eq!(parse("use 'other'").unwrap(), Statement::Use("other".into()));
        assert_eq!(parse("db.users.find()").unwrap(), collection("users", vec![c("find", &[])]));
        assert_eq!(
            parse("db.users.find({a: 1, b: [1, 2]}, {a: 1}).sort({a: -1}).limit(5).skip(2)").unwrap(),
            collection(
                "users",
                vec![c("find", &["{a: 1, b: [1, 2]}", "{a: 1}"]), c("sort", &["{a: -1}"]), c("limit", &["5"]), c("skip", &["2"])]
            )
        );
        assert_eq!(parse(r#"db.getCollection("my-things").drop()"#).unwrap(), collection("my-things", vec![c("drop", &[])]));
        assert_eq!(parse(r#"db["odd name"].findOne({})"#).unwrap(), collection("odd name", vec![c("findOne", &["{}"])]));
        assert_eq!(parse("db.system.users.find()").unwrap(), collection("system.users", vec![c("find", &[])]));
        assert_eq!(
            parse("db.runCommand({ping: 1})").unwrap(),
            Statement::Database { method: "runCommand".into(), arguments: vec!["{ping: 1}".into()] }
        );
        assert_eq!(
            parse("db.getCollectionNames()").unwrap(),
            Statement::Database { method: "getCollectionNames".into(), arguments: vec![] }
        );
        assert_eq!(
            parse(r#"db.users.distinct("city", {a: ")"})"#).unwrap(),
            collection("users", vec![c("distinct", &[r#""city""#, r#"{a: ")"}"#])])
        );
        assert_eq!(
            parse("db.users.insertMany([{a: 1}, {a: 2},])").unwrap(),
            collection("users", vec![c("insertMany", &["[{a: 1}, {a: 2},]"])])
        );
        assert_eq!(parse("db.users.find({a: /x)/})").unwrap(), collection("users", vec![c("find", &["{a: /x)/}"])]));
        assert_eq!(parse("db.users.find(1, 2,)").unwrap(), collection("users", vec![c("find", &["1", "2"])]));
        for text in [
            "SELECT 1",
            "db",
            "db.users",
            "db.users.find(",
            "show",
            "show users x",
            "db.users.find() extra",
            "users.find()",
            "db.runCommand({}).foo()",
            "print('x')",
            "use",
        ] {
            assert!(parse(text).is_err(), "{text}");
        }
        assert_eq!(parse("print('x')").unwrap_err().message(), "dbLens can't run this yet: print('x')");
        assert_eq!(parse("foo(1)\nbar").unwrap_err().message(), "dbLens can't run this yet: foo(1)");
    }

    #[test]
    fn reads_literals_as_extended_json() {
        let cases: &[(&str, &str)] = &[
            ("{}", "{}"),
            ("[]", "[]"),
            ("{a: 1}", r#"{"a":1}"#),
            ("{ 'a b': 'c' , $set : {x:true}, }", r#"{"a b":"c","$set":{"x":true}}"#),
            (r#"{"a": "b", "n": null}"#, r#"{"a":"b","n":null}"#),
            ("{a: [1, 2.5, -3, 1e3, .5, 5., +7, 0x1F, 007,]}", r#"{"a":[1,2.5,-3,1e3,0.5,5.0,7,31,7]}"#),
            (r#"{s: 'it\'s', d: "say \"hi\"", t: `tpl`}"#, r#"{"s":"it's","d":"say \"hi\"","t":"tpl"}"#),
            (r"{u: 'é\x41\n\t\\'}", r#"{"u":"éA\n\t\\"}"#),
            ("{e: 'café ☕'}", r#"{"e":"café ☕"}"#),
            ("{p: '😀'}", r#"{"p":"😀"}"#),
            (r"{p: '😀', q: '\u{1F600}'}", r#"{"p":"😀","q":"😀"}"#),
            (r#"{_id: ObjectId("65A1b2c3d4e5f60718293a4b")}"#, r#"{"_id":{"$oid":"65a1b2c3d4e5f60718293a4b"}}"#),
            (r#"{d: ISODate("2024-05-01T10:30:00.250Z")}"#, r#"{"d":{"$date":{"$numberLong":"1714559400250"}}}"#),
            (r#"{d: new Date("2024-05-01")}"#, r#"{"d":{"$date":{"$numberLong":"1714521600000"}}}"#),
            ("{d: new Date(0)}", r#"{"d":{"$date":{"$numberLong":"0"}}}"#),
            (r#"{d: Date("1969-12-31T23:59:59.999Z")}"#, r#"{"d":{"$date":{"$numberLong":"-1"}}}"#),
            ("{n: NumberLong(5)}", r#"{"n":{"$numberLong":"5"}}"#),
            (r#"{n: NumberLong("9007199254740993")}"#, r#"{"n":{"$numberLong":"9007199254740993"}}"#),
            ("{n: NumberInt(42)}", r#"{"n":{"$numberInt":"42"}}"#),
            (r#"{n: NumberDecimal("9.99")}"#, r#"{"n":{"$numberDecimal":"9.99"}}"#),
            (r#"{n: Decimal128("1E+3")}"#, r#"{"n":{"$numberDecimal":"1E+3"}}"#),
            ("{a: true, b: false, c: null, d: undefined}", r#"{"a":true,"b":false,"c":null,"d":{"$undefined":true}}"#),
            (
                "{a: Infinity, b: -Infinity, c: NaN}",
                r#"{"a":{"$numberDouble":"Infinity"},"b":{"$numberDouble":"-Infinity"},"c":{"$numberDouble":"NaN"}}"#,
            ),
            ("{name: /^an.a$/i}", r#"{"name":{"$regularExpression":{"pattern":"^an.a$","options":"i"}}}"#),
            (r"{name: /a\/b[/]c/gmi}", r#"{"name":{"$regularExpression":{"pattern":"a\\/b[/]c","options":"im"}}}"#),
            (r#"{r: RegExp("x+", "s")}"#, r#"{"r":{"$regularExpression":{"pattern":"x+","options":"s"}}}"#),
            ("{k: MinKey, m: MaxKey()}", r#"{"k":{"$minKey":1},"m":{"$maxKey":1}}"#),
            ("{t: Timestamp(1700000000, 1)}", r#"{"t":{"$timestamp":{"t":1700000000,"i":1}}}"#),
            ("{t: Timestamp({t: 1, i: 2})}", r#"{"t":{"$timestamp":{"t":1,"i":2}}}"#),
            (r#"{b: BinData(0, "AQID")}"#, r#"{"b":{"$binary":{"base64":"AQID","subType":"00"}}}"#),
            (
                r#"{u: UUID("0e4f7a3c-1b2d-4e5f-8a9b-0c1d2e3f4a5b")}"#,
                r#"{"u":{"$binary":{"base64":"Dk96PBstTl+KmwwdLj9KWw==","subType":"04"}}}"#,
            ),
            (r#"{x: {"$oid": "65a1b2c3d4e5f60718293a4b"}}"#, r#"{"x":{"$oid":"65a1b2c3d4e5f60718293a4b"}}"#),
            (r#"{"a.b": 1, c.d: 2}"#, r#"{"a.b":1,"c.d":2}"#),
            ("{a: 1, // one\n b: /* two */ 2}", r#"{"a":1,"b":2}"#),
            (
                "[{$match: {a: {$in: [1,2]}}}, {$group: {_id: '$a', n: {$sum: 1}}}]",
                r#"[{"$match":{"a":{"$in":[1,2]}}},{"$group":{"_id":"$a","n":{"$sum":1}}}]"#,
            ),
            ("'plain'", r#""plain""#),
            ("42", "42"),
            ("-0.5", "-0.5"),
            ("1_000", "1000"),
        ];
        for (literal, json) in cases {
            assert_eq!(extended_json(literal).unwrap(), *json, "{literal}");
        }
    }

    #[test]
    fn helpers_make_values() {
        let id = extended_json("ObjectId()").unwrap();
        assert!(Regex::new(r#"^\{"\$oid":"[0-9a-f]{24}"\}$"#).unwrap().is_match(&id), "{id}");
        assert!(extended_json("new Date()").unwrap().starts_with(r#"{"$date":{"$numberLong":""#));
        assert!(extended_json("UUID()").unwrap().ends_with(r#""subType":"04"}}"#));
    }

    #[test]
    fn refuses_bad_literals() {
        for literal in [
            "{a: }",
            "{a 1}",
            "{a: 1",
            "[1, 2",
            "{a: foo}",
            "{a: ObjectId('xyz')}",
            "{a: ISODate('yesterday')}",
            "{a: NumberInt('1.5')}",
            "{a: NumberInt(3000000000)}",
            "{a: NumberDecimal('abc')}",
            "{a: Foo(1)}",
            "{a: 'unterminated}",
            "{a: 1} extra",
            "",
            "{a: 1,,}",
            "{a: 'line\nbreak'}",
            "{a: BinData(0, 'not base64!')}",
        ] {
            assert!(extended_json(literal).is_err(), "{literal}");
        }
        assert_eq!(extended_json("{a: ObjectId('xyz')}").unwrap_err().message(), r#"This isn't a valid ObjectId("xyz")."#);
        assert_eq!(extended_json("{a: Foo(1)}").unwrap_err().message(), "dbLens can't read Foo(…) here.");
        assert_eq!(extended_json("{a: 1} extra").unwrap_err().message(), "This isn't a value dbLens can read: {a: 1} extra");
        let long = format!("{{a: '{}", "x".repeat(100));
        assert!(extended_json(&long).unwrap_err().message().ends_with("xxx…"));
    }

    #[test]
    fn reads_iso_dates() {
        assert_eq!(iso_date_milliseconds("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(iso_date_milliseconds("2024-05-01T10:30:00.250Z"), Some(1_714_559_400_250));
        assert_eq!(iso_date_milliseconds("2024-05-01T12:30:00.250+02:00"), Some(1_714_559_400_250));
        assert_eq!(iso_date_milliseconds("2024-05-01 10:30:00.250"), Some(1_714_559_400_250));
        assert_eq!(iso_date_milliseconds("2024-05-01T10:30Z"), Some(1_714_559_400_000));
        assert_eq!(iso_date_milliseconds("2024-02-29"), Some(1_709_164_800_000));
        assert_eq!(iso_date_milliseconds("2023-02-29"), None);
        assert_eq!(iso_date_milliseconds("2024-13-01"), None);
        assert_eq!(iso_date_milliseconds("May 1"), None);
        assert_eq!(iso_date(1_714_559_400_250).as_deref(), Some("2024-05-01T10:30:00.250Z"));
        assert_eq!(iso_date(-1).as_deref(), Some("1969-12-31T23:59:59.999Z"));
        assert_eq!(iso_date(0).as_deref(), Some("1970-01-01T00:00:00.000Z"));
        assert_eq!(iso_date(253_402_300_800_000), None);
    }

    #[test]
    fn reads_string_arguments_and_numbers() {
        assert_eq!(string_argument(" 'city' ").as_deref(), Some("city"));
        assert_eq!(string_argument("`a b`").as_deref(), Some("a b"));
        assert_eq!(string_argument("city"), None);
        assert_eq!(string_argument("'a' 'b'"), None);
        assert_eq!(json_string("a\"b\\\n\u{1}"), "\"a\\\"b\\\\\\n\\u0001\"");
        assert_eq!(double_text(1.5), "1.5");
        assert_eq!(double_text(3.0), "3.0");
        assert_eq!(double_text(1e20), "1e+20");
        assert_eq!(double_text(1.5e-7), "1.5e-07");
        assert_eq!(double_text(f64::NEG_INFINITY), "-Infinity");
        assert!(is_decimal("9.99") && is_decimal("1E+3") && !is_decimal("abc") && !is_decimal(""));
    }
}
