//! A result as a file: CSV (RFC 4180) or JSON (an array of objects). Port of `Export.swift`.

use crate::session::QueryResult;

/// Header line, then one line per row. Values with a comma, quote, or line
/// break go in quotes; NULL is an empty field.
pub fn csv(result: &QueryResult) -> String {
    fn field(value: Option<&str>) -> String {
        match value {
            None => String::new(),
            Some(v) if v.contains([',', '"', '\n', '\r']) => {
                format!("\"{}\"", v.replace('"', "\"\""))
            }
            Some(v) => v.to_string(),
        }
    }
    let mut lines = vec![result
        .columns
        .iter()
        .map(|c| field(Some(&c.name)))
        .collect::<Vec<_>>()
        .join(",")];
    lines.extend(result.rows.iter().map(|row| {
        row.iter()
            .map(|v| field(v.as_deref()))
            .collect::<Vec<_>>()
            .join(",")
    }));
    lines.join("\r\n") + "\r\n"
}

/// One object per row, keyed by column name, in column order. Values stay text
/// as the grid shows them; NULL is null.
pub fn json(result: &QueryResult) -> String {
    let string = |text: &str| serde_json::to_string(text).unwrap_or_else(|_| "\"\"".into());
    let names: Vec<String> = result.columns.iter().map(|c| string(&c.name)).collect();
    let objects: Vec<String> = result
        .rows
        .iter()
        .map(|row| {
            let fields: Vec<String> = names
                .iter()
                .zip(row)
                .map(|(name, value)| {
                    format!(
                        "{name}: {}",
                        value
                            .as_deref()
                            .map(string)
                            .unwrap_or_else(|| "null".into())
                    )
                })
                .collect();
            format!("  {{{}}}", fields.join(", "))
        })
        .collect();
    if objects.is_empty() {
        "[]\n".into()
    } else {
        format!("[\n{}\n]\n", objects.join(",\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Column;

    fn sample() -> QueryResult {
        QueryResult::rows(
            vec![Column::new("id", ""), Column::new("note", "")],
            vec![
                vec![Some("1".into()), Some("a, \"b\"".into())],
                vec![Some("2".into()), None],
            ],
            "SELECT 2",
        )
    }

    #[test]
    fn writes_csv() {
        assert_eq!(csv(&sample()), "id,note\r\n1,\"a, \"\"b\"\"\"\r\n2,\r\n");
    }

    #[test]
    fn writes_json() {
        assert_eq!(json(&sample()), "[\n  {\"id\": \"1\", \"note\": \"a, \\\"b\\\"\"},\n  {\"id\": \"2\", \"note\": null}\n]\n");
        assert_eq!(json(&QueryResult::rows(vec![], vec![], "SELECT 0")), "[]\n");
    }
}
