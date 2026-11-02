//! Table data over the IPC: MessagePack, column-major, never JSON.
//!
//! A result block is a map (see CONTRACT.md, "Binary results"):
//!
//! ```text
//! { columns: [{name, type}], data: [[v, …] per column], rowCount, offset,
//!   command, truncated, totalRows, keys: null | {columns, kind, data}, note }
//! ```
//!
//! Each `v` is nil (NULL), a string (the server's text), or an integer when the
//! text is an integer's canonical form that a JS number holds exactly (so
//! `String(v)` gives the text back, byte for byte). Key values are always
//! strings: they go back to the driver as they came.
//!
//! `StreamSink` sends a streamed run as a sequence of such messages over a
//! `tauri::ipc::Channel` (`execute_stream`).

use std::sync::Arc;
use std::time::Instant;

use rmp::encode;

use crate::session::{Column, DbError, Execution, QueryResult, RowKeyKind, RowKeys};

/// Rows in the first block of a streamed run, so something shows at once.
pub const FIRST_BLOCK_ROWS: usize = 500;
/// Rows in each later block (a block is also sent when it holds this many bytes
/// or rows have waited `FLUSH_MS`).
pub const BLOCK_ROWS: usize = 5_000;
const BLOCK_BYTES: usize = 2 << 20;
const FLUSH_MS: u128 = 200;
/// Rows a streamed run keeps by default; more are counted, not sent.
pub const STREAM_ROW_CAP: usize = 100_000;

// MARK: - Values

/// The integer `text` is, when `String(n)` in JS gives `text` back exactly:
/// no sign on zero, no leading zeros, within ±(2^53 - 1).
pub fn safe_int(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    let (negative, digits) = match bytes.first()? {
        b'-' => (true, &bytes[1..]),
        _ => (false, bytes),
    };
    if digits.is_empty() || digits.len() > 16 || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    if digits[0] == b'0' && (digits.len() > 1 || negative) {
        return None;
    }
    let mut n: i64 = 0;
    for d in digits {
        n = n * 10 + i64::from(d - b'0');
    }
    const MAX: i64 = (1 << 53) - 1;
    if n > MAX {
        return None;
    }
    Some(if negative { -n } else { n })
}

/// Encodes the values of one column, trying integers until one value isn't.
struct ColumnWriter {
    try_int: bool,
}

impl ColumnWriter {
    fn new() -> Self {
        Self { try_int: true }
    }

    fn value(&mut self, out: &mut Vec<u8>, value: Option<&str>) {
        match value {
            None => {
                let _ = encode::write_nil(out);
            }
            Some(text) => {
                if self.try_int {
                    if let Some(n) = safe_int(text) {
                        let _ = encode::write_sint(out, n);
                        return;
                    }
                    self.try_int = false;
                }
                let _ = encode::write_str(out, text);
            }
        }
    }
}

fn str_or_nil(out: &mut Vec<u8>, value: Option<&str>) {
    match value {
        Some(text) => {
            let _ = encode::write_str(out, text);
        }
        None => {
            let _ = encode::write_nil(out);
        }
    }
}

fn key(out: &mut Vec<u8>, name: &str) {
    let _ = encode::write_str(out, name);
}

fn columns(out: &mut Vec<u8>, columns: &[Column]) {
    let _ = encode::write_array_len(out, columns.len() as u32);
    for column in columns {
        let _ = encode::write_map_len(out, 2);
        key(out, "name");
        let _ = encode::write_str(out, &column.name);
        key(out, "type");
        let _ = encode::write_str(out, &column.type_name);
    }
}

fn kind_name(kind: RowKeyKind) -> &'static str {
    match kind {
        RowKeyKind::PrimaryKey => "primaryKey",
        RowKeyKind::Ctid => "ctid",
        RowKeyKind::Rowid => "rowid",
    }
}

fn keys(out: &mut Vec<u8>, keys: Option<&RowKeys>) {
    let Some(keys) = keys else {
        let _ = encode::write_nil(out);
        return;
    };
    let _ = encode::write_map_len(out, 3);
    key(out, "columns");
    let _ = encode::write_array_len(out, keys.columns.len() as u32);
    for name in &keys.columns {
        let _ = encode::write_str(out, name);
    }
    key(out, "kind");
    let _ = encode::write_str(out, kind_name(keys.kind));
    key(out, "data");
    let _ = encode::write_array_len(out, keys.columns.len() as u32);
    for c in 0..keys.columns.len() {
        let _ = encode::write_array_len(out, keys.values.len() as u32);
        for row in &keys.values {
            str_or_nil(out, row.get(c).and_then(|v| v.as_deref()));
        }
    }
}

// MARK: - Blocks

/// Appends one result as a block map. `offset` is where its first row sits in
/// the table (0 for SQL results).
pub fn write_block(out: &mut Vec<u8>, result: &QueryResult, offset: u64) {
    let _ = encode::write_map_len(out, 9);
    key(out, "columns");
    columns(out, &result.columns);
    key(out, "data");
    let width = result.columns.len();
    let _ = encode::write_array_len(out, width as u32);
    for c in 0..width {
        let _ = encode::write_array_len(out, result.rows.len() as u32);
        let mut writer = ColumnWriter::new();
        for row in &result.rows {
            writer.value(out, row.get(c).and_then(|v| v.as_deref()));
        }
    }
    key(out, "rowCount");
    let _ = encode::write_uint(out, result.rows.len() as u64);
    key(out, "offset");
    let _ = encode::write_uint(out, offset);
    key(out, "command");
    let _ = encode::write_str(out, &result.command);
    key(out, "truncated");
    let _ = encode::write_bool(out, result.truncated);
    key(out, "totalRows");
    let _ = encode::write_uint(out, result.total_rows);
    key(out, "keys");
    keys(out, result.keys.as_ref());
    key(out, "note");
    str_or_nil(out, result.note.as_deref());
}

/// A table block (`page`, `page_block`).
pub fn block(result: &QueryResult, offset: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(estimate(result));
    write_block(&mut out, result, offset);
    out
}

fn estimate(result: &QueryResult) -> usize {
    let sample: usize = result
        .rows
        .first()
        .map(|r| {
            r.iter()
                .map(|v| v.as_ref().map_or(1, |s| s.len() + 2))
                .sum()
        })
        .unwrap_or(0);
    256 + sample * result.rows.len() * 11 / 10
}

fn write_execution(out: &mut Vec<u8>, execution: &Execution) {
    let _ = encode::write_map_len(out, 2);
    key(out, "results");
    let _ = encode::write_array_len(out, execution.results.len() as u32);
    for result in &execution.results {
        write_block(out, result, 0);
    }
    key(out, "durationMs");
    let _ = encode::write_f64(out, execution.duration_ms);
}

/// `{ results: [block…], durationMs }` (`execute`, `read_only`, `rehearse`).
pub fn execution(execution: &Execution) -> Vec<u8> {
    let mut out = Vec::with_capacity(execution.results.iter().map(estimate).sum::<usize>() + 64);
    write_execution(&mut out, execution);
    out
}

/// `{ kind: "ran", execution }` (`execute_atomically`).
pub fn atomic_ran(run: &Execution) -> Vec<u8> {
    let mut out = Vec::new();
    let _ = encode::write_map_len(&mut out, 2);
    key(&mut out, "kind");
    let _ = encode::write_str(&mut out, "ran");
    key(&mut out, "execution");
    write_execution(&mut out, run);
    out
}

/// `{ kind: "notAtomic", message }` (`execute_atomically`).
pub fn atomic_refused(message: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let _ = encode::write_map_len(&mut out, 2);
    key(&mut out, "kind");
    let _ = encode::write_str(&mut out, "notAtomic");
    key(&mut out, "message");
    let _ = encode::write_str(&mut out, message);
    out
}

// MARK: - Streaming

/// Sends one message; false when nobody listens any more (the window closed).
pub type Sender = Arc<dyn Fn(Vec<u8>) -> bool + std::marker::Send + Sync>;

/// Where a streamed run sends its rows: the driver calls `begin` for each
/// result set, `cell` for each value and `end_row` after each row, then `end`
/// (or just `command` for a statement without rows). Rows go out in blocks:
/// the first after `FIRST_BLOCK_ROWS`, then every `BLOCK_ROWS`. After `cap`
/// rows of a result, rows are only counted.
///
/// Messages (each one MessagePack map, `type` first):
/// `columns {result, columns}`, `rows {result, offset, rowCount, data}`,
/// `end {result, command, totalRows, truncated}`, `done {durationMs, error, cancelled}`.
pub struct StreamSink {
    send: Sender,
    cap: usize,
    open: bool,
    result: u32,
    width: usize,
    /// One encoded value buffer per column for the rows not sent yet.
    buffers: Vec<Vec<u8>>,
    writers: Vec<ColumnWriter>,
    at: usize,
    pending: usize,
    /// Rows sent (or pending) of the current result, and rows seen.
    kept: usize,
    seen: u64,
    sent_first: bool,
    last_flush: Instant,
    /// Bytes and messages sent, for the bench.
    pub bytes: usize,
    pub messages: usize,
    /// When the first rows went out, for the bench.
    pub first_rows_at: Option<Instant>,
}

impl StreamSink {
    pub fn new(send: Sender, cap: usize) -> Self {
        Self {
            send,
            cap: cap.max(1),
            open: true,
            result: 0,
            width: 0,
            buffers: vec![],
            writers: vec![],
            at: 0,
            pending: 0,
            kept: 0,
            seen: 0,
            sent_first: false,
            last_flush: Instant::now(),
            bytes: 0,
            messages: 0,
            first_rows_at: None,
        }
    }

    /// A sink with nobody listening, as a placeholder.
    pub fn closed() -> Self {
        let mut sink = Self::new(Arc::new(|_| false), 1);
        sink.open = false;
        sink
    }

    /// False once the receiver is gone: the driver should stop reading.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Whether the next row's values are kept (false past the cap): the
    /// driver may skip `cell` calls then, but still calls `end_row`.
    pub fn keeping(&self) -> bool {
        self.open && self.kept < self.cap
    }

    /// Rows seen in the current result set.
    pub fn seen(&self) -> u64 {
        self.seen
    }

    fn emit(&mut self, message: Vec<u8>) {
        if !self.open {
            return;
        }
        self.bytes += message.len();
        self.messages += 1;
        if !(self.send)(message) {
            self.open = false;
        }
    }

    fn head(out: &mut Vec<u8>, entries: u32, kind: &str, result: Option<u32>) {
        let _ = encode::write_map_len(out, entries);
        key(out, "type");
        let _ = encode::write_str(out, kind);
        if let Some(result) = result {
            key(out, "result");
            let _ = encode::write_uint(out, u64::from(result));
        }
    }

    /// A result set with these columns starts.
    pub fn begin(&mut self, cols: &[Column]) {
        self.width = cols.len();
        self.buffers = (0..self.width).map(|_| Vec::with_capacity(4096)).collect();
        self.writers = (0..self.width).map(|_| ColumnWriter::new()).collect();
        self.at = 0;
        self.pending = 0;
        self.kept = 0;
        self.seen = 0;
        self.sent_first = false;
        self.last_flush = Instant::now();
        let mut out = Vec::with_capacity(64 + cols.len() * 32);
        Self::head(&mut out, 3, "columns", Some(self.result));
        key(&mut out, "columns");
        columns(&mut out, cols);
        self.emit(out);
    }

    /// The next value of the current row (NULL is None).
    pub fn cell(&mut self, value: Option<&str>) {
        if self.kept >= self.cap || self.at >= self.width {
            return;
        }
        self.writers[self.at].value(&mut self.buffers[self.at], value);
        self.at += 1;
    }

    /// The current row is complete.
    pub fn end_row(&mut self) {
        self.seen += 1;
        if self.kept < self.cap && self.open {
            // A short row (fewer values than columns) gets NULLs.
            while self.at < self.width {
                let _ = encode::write_nil(&mut self.buffers[self.at]);
                self.at += 1;
            }
            self.kept += 1;
            self.pending += 1;
            let limit = if self.sent_first {
                BLOCK_ROWS
            } else {
                FIRST_BLOCK_ROWS
            };
            if self.pending >= limit
                || (self.pending % 256 == 0
                    && (self.buffered() >= BLOCK_BYTES
                        || self.last_flush.elapsed().as_millis() >= FLUSH_MS))
            {
                self.flush();
            }
        }
        self.at = 0;
    }

    fn buffered(&self) -> usize {
        self.buffers.iter().map(Vec::len).sum()
    }

    /// Sends the pending rows as one `rows` message.
    pub fn flush(&mut self) {
        if self.pending == 0 {
            return;
        }
        let mut out = Vec::with_capacity(self.buffered() + 64 + self.width * 5);
        Self::head(&mut out, 5, "rows", Some(self.result));
        key(&mut out, "offset");
        let _ = encode::write_uint(&mut out, (self.kept - self.pending) as u64);
        key(&mut out, "rowCount");
        let _ = encode::write_uint(&mut out, self.pending as u64);
        key(&mut out, "data");
        let _ = encode::write_array_len(&mut out, self.width as u32);
        for buffer in &mut self.buffers {
            let _ = encode::write_array_len(&mut out, self.pending as u32);
            out.extend_from_slice(buffer);
            buffer.clear();
        }
        self.pending = 0;
        self.sent_first = true;
        self.last_flush = Instant::now();
        if self.first_rows_at.is_none() {
            self.first_rows_at = Some(Instant::now());
        }
        self.emit(out);
    }

    /// The current result set is complete: what the statement did and how many rows it had.
    pub fn end(&mut self, command: &str) {
        self.flush();
        let total = self.seen;
        let truncated = total > self.kept as u64;
        self.finish(command, total, truncated);
    }

    /// A statement without rows ("UPDATE 2", "CREATE TABLE").
    pub fn command(&mut self, command: &str, affected: u64) {
        self.finish(command, affected, false);
    }

    fn finish(&mut self, command: &str, total: u64, truncated: bool) {
        let mut out = Vec::with_capacity(96 + command.len());
        Self::head(&mut out, 5, "end", Some(self.result));
        key(&mut out, "command");
        let _ = encode::write_str(&mut out, command);
        key(&mut out, "totalRows");
        let _ = encode::write_uint(&mut out, total);
        key(&mut out, "truncated");
        let _ = encode::write_bool(&mut out, truncated);
        self.emit(out);
        self.result += 1;
        self.width = 0;
        self.buffers.clear();
        self.writers.clear();
        self.kept = 0;
        self.seen = 0;
    }

    /// A whole result from a driver that doesn't stream.
    pub fn send_result(&mut self, result: &QueryResult) {
        if !result.has_rows() {
            self.command(&result.command, result.total_rows);
            return;
        }
        self.begin(&result.columns);
        for row in &result.rows {
            if self.keeping() {
                for value in row {
                    self.cell(value.as_deref());
                }
            }
            self.end_row();
        }
        self.flush();
        // The driver may have cut the rows already: its counts stand.
        let truncated = result.truncated || result.total_rows > self.kept as u64;
        self.finish(&result.command, result.total_rows.max(self.seen), truncated);
    }

    /// The last message: the run's wall time, and its error if it failed.
    pub fn done(&mut self, duration_ms: f64, error: Option<&DbError>) {
        self.flush();
        let mut out = Vec::with_capacity(96);
        Self::head(&mut out, 4, "done", None);
        key(&mut out, "durationMs");
        let _ = encode::write_f64(&mut out, duration_ms);
        key(&mut out, "error");
        str_or_nil(&mut out, error.map(|e| e.message()).as_deref());
        key(&mut out, "cancelled");
        let _ = encode::write_bool(&mut out, matches!(error, Some(DbError::Cancelled)));
        self.emit(out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn decode(bytes: &[u8]) -> serde_json::Value {
        rmp_serde::from_slice(bytes).expect("valid MessagePack")
    }

    #[test]
    fn integers_only_when_lossless() {
        assert_eq!(safe_int("0"), Some(0));
        assert_eq!(safe_int("-42"), Some(-42));
        assert_eq!(safe_int("9007199254740991"), Some(9_007_199_254_740_991));
        for text in [
            "",
            "-",
            "-0",
            "007",
            "1.0",
            "1e3",
            " 1",
            "+1",
            "9007199254740992",
            "12345678901234567",
        ] {
            assert_eq!(safe_int(text), None, "{text:?}");
        }
    }

    #[test]
    fn blocks_are_column_major() {
        let s = |v: &str| Some(v.to_string());
        let mut result = QueryResult::rows(
            vec![Column::new("id", "int4"), Column::new("name", "text")],
            vec![
                vec![s("1"), s("ana")],
                vec![s("2"), None],
                vec![s("x"), s("007")],
            ],
            "SELECT 3",
        );
        result.keys = Some(RowKeys {
            columns: vec!["id".into()],
            kind: RowKeyKind::PrimaryKey,
            values: vec![vec![s("1")], vec![s("2")], vec![s("x")]],
        });
        let value = decode(&block(&result, 200));
        assert_eq!(value["columns"][1]["type"], "text");
        assert_eq!(value["data"][0], serde_json::json!([1, 2, "x"]));
        assert_eq!(value["data"][1], serde_json::json!(["ana", null, "007"]));
        assert_eq!(value["rowCount"], 3);
        assert_eq!(value["offset"], 200);
        assert_eq!(value["keys"]["kind"], "primaryKey");
        assert_eq!(value["keys"]["data"][0], serde_json::json!(["1", "2", "x"]));
        assert_eq!(value["note"], serde_json::Value::Null);
    }

    #[test]
    fn streams_in_blocks_up_to_the_cap() {
        let messages = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
        let kept = messages.clone();
        let mut sink = StreamSink::new(
            Arc::new(move |m| {
                kept.lock().unwrap().push(m);
                true
            }),
            FIRST_BLOCK_ROWS + 10,
        );
        sink.begin(&[Column::new("n", "")]);
        for i in 0..FIRST_BLOCK_ROWS + 100 {
            if sink.keeping() {
                sink.cell(Some(&i.to_string()));
            }
            sink.end_row();
        }
        sink.end("SELECT 600");
        sink.command("UPDATE 1", 1);
        sink.done(1.5, None);
        let all: Vec<serde_json::Value> =
            messages.lock().unwrap().iter().map(|m| decode(m)).collect();
        let kinds: Vec<&str> = all.iter().map(|m| m["type"].as_str().unwrap()).collect();
        assert_eq!(kinds, ["columns", "rows", "rows", "end", "end", "done"]);
        assert_eq!(all[1]["rowCount"], FIRST_BLOCK_ROWS);
        assert_eq!(all[2]["offset"], FIRST_BLOCK_ROWS);
        assert_eq!(all[2]["rowCount"], 10);
        assert_eq!(all[3]["totalRows"], FIRST_BLOCK_ROWS + 100);
        assert_eq!(all[3]["truncated"], true);
        assert_eq!(all[4]["result"], 1);
        assert_eq!(all[5]["error"], serde_json::Value::Null);
    }
}
