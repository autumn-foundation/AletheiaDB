//! Row readers (CSV / JSONL) and value coercion for the bulk importer.
//!
//! Both readers yield a uniform stream of `Result<Row, ImportError>` so the import
//! loop in [`super`] is format-agnostic. A [`Row`] holds raw [`Cell`]s keyed by
//! column name; coercion to [`PropertyValue`] happens lazily, driven by the caller's
//! [`ColumnType`](super::mapping::ColumnType) mapping.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, Utc};

use super::mapping::ColumnType;
use super::{ImportError, RowError};
use crate::core::property::PropertyValue;
use crate::core::temporal::Timestamp;

/// A raw cell value, before coercion. CSV cells are always strings; JSONL cells
/// preserve their parsed JSON type.
#[derive(Debug, Clone)]
pub(crate) enum Cell {
    Str(String),
    Json(serde_json::Value),
}

/// One parsed input row: the 1-based row/line number plus its named cells.
#[derive(Debug, Clone)]
pub(crate) struct Row {
    pub(crate) index: usize,
    pub(crate) cells: HashMap<String, Cell>,
}

/// A boxed, streaming iterator of parsed rows.
pub(crate) type RowIter = Box<dyn Iterator<Item = Result<Row, ImportError>>>;

impl Row {
    /// Fetch a cell as a string, erroring if the column is absent.
    pub(crate) fn get_str(&self, column: &str) -> Result<String, String> {
        match self.cells.get(column) {
            Some(cell) => Ok(cell_to_string(cell)),
            None => Err(format!("missing column '{column}'")),
        }
    }
}

/// Render a cell as a plain string (used for labels, keys, and valid-time parsing).
fn cell_to_string(cell: &Cell) -> String {
    match cell {
        Cell::Str(s) => s.clone(),
        Cell::Json(serde_json::Value::String(s)) => s.clone(),
        Cell::Json(serde_json::Value::Null) => String::new(),
        Cell::Json(other) => other.to_string(),
    }
}

/// Coerce a raw cell to a [`PropertyValue`] per the requested [`ColumnType`].
///
/// An empty value coerces to [`PropertyValue::Null`] for the non-string types so that
/// blank cells become "absent" rather than a hard error.
pub(crate) fn coerce(cell: &Cell, ty: ColumnType) -> Result<PropertyValue, String> {
    match cell {
        Cell::Str(s) => coerce_str(s, ty),
        Cell::Json(value) => coerce_json(value, ty),
    }
}

fn coerce_str(raw: &str, ty: ColumnType) -> Result<PropertyValue, String> {
    let trimmed = raw.trim();
    match ty {
        ColumnType::String => Ok(PropertyValue::string(raw)),
        ColumnType::Int => {
            if trimmed.is_empty() {
                return Ok(PropertyValue::Null);
            }
            trimmed
                .parse::<i64>()
                .map(PropertyValue::Int)
                .map_err(|_| format!("cannot coerce '{raw}' to Int"))
        }
        ColumnType::Float => {
            if trimmed.is_empty() {
                return Ok(PropertyValue::Null);
            }
            trimmed
                .parse::<f64>()
                .map(PropertyValue::Float)
                .map_err(|_| format!("cannot coerce '{raw}' to Float"))
        }
        ColumnType::Bool => {
            if trimmed.is_empty() {
                return Ok(PropertyValue::Null);
            }
            parse_bool(trimmed)
                .map(PropertyValue::Bool)
                .ok_or_else(|| format!("cannot coerce '{raw}' to Bool"))
        }
    }
}

fn coerce_json(value: &serde_json::Value, ty: ColumnType) -> Result<PropertyValue, String> {
    use serde_json::Value;
    match (ty, value) {
        (_, Value::Null) => Ok(PropertyValue::Null),
        (ColumnType::String, Value::String(s)) => Ok(PropertyValue::string(s)),
        (ColumnType::String, other) => Ok(PropertyValue::string(other.to_string())),
        (ColumnType::Int, Value::Number(n)) => n
            .as_i64()
            .map(PropertyValue::Int)
            .ok_or_else(|| format!("cannot coerce '{n}' to Int")),
        (ColumnType::Int, Value::String(s)) => coerce_str(s, ColumnType::Int),
        (ColumnType::Float, Value::Number(n)) => n
            .as_f64()
            .map(PropertyValue::Float)
            .ok_or_else(|| format!("cannot coerce '{n}' to Float")),
        (ColumnType::Float, Value::String(s)) => coerce_str(s, ColumnType::Float),
        (ColumnType::Bool, Value::Bool(b)) => Ok(PropertyValue::Bool(*b)),
        (ColumnType::Bool, Value::String(s)) => coerce_str(s, ColumnType::Bool),
        (ty, other) => Err(format!("cannot coerce JSON {other} to {ty:?}")),
    }
}

fn parse_bool(s: &str) -> Option<bool> {
    match s.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "t" => Some(true),
        "false" | "0" | "no" | "f" => Some(false),
        _ => None,
    }
}

/// Parse a per-row valid-time string into a [`Timestamp`].
///
/// Accepts (in priority order): bare integer microseconds since the Unix epoch,
/// RFC 3339 / ISO 8601 with `Z` or a numeric offset, a naive datetime (assumed UTC),
/// and a bare date (`YYYY-MM-DD`, midnight UTC). Mirrors the SQL temporal parser so
/// imported timestamps round-trip with `AS OF` queries.
pub(crate) fn parse_valid_time(s: &str) -> Result<Timestamp, String> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err("empty valid_time value".to_string());
    }

    // Bare integer => microseconds since epoch (matches SQL temporal parser).
    if let Ok(micros) = trimmed.parse::<i64>() {
        return Ok(Timestamp::from(micros));
    }
    if let Ok(dt) = trimmed.parse::<DateTime<Utc>>() {
        return Ok(Timestamp::from(dt.timestamp_micros()));
    }
    if let Ok(dt) = trimmed.parse::<DateTime<FixedOffset>>() {
        return Ok(Timestamp::from(dt.with_timezone(&Utc).timestamp_micros()));
    }
    if let Ok(dt) = trimmed.parse::<NaiveDateTime>() {
        return Ok(Timestamp::from(dt.and_utc().timestamp_micros()));
    }
    if let Ok(date) = trimmed.parse::<NaiveDate>() {
        let dt = date
            .and_hms_opt(0, 0, 0)
            .ok_or_else(|| format!("invalid date '{s}'"))?;
        return Ok(Timestamp::from(dt.and_utc().timestamp_micros()));
    }

    Err(format!("could not parse valid_time '{s}'"))
}

/// Build a streaming row reader over a CSV file (first record is the header).
pub(crate) fn csv_rows(path: &Path) -> Result<RowIter, ImportError> {
    csv_rows_with(path, b',', b'"')
}

/// Build a streaming row reader over a CSV file with a configurable field
/// delimiter and quote character (Issue #3356).
///
/// The Neo4j importer supplies the delimiter/quote from
/// [`Neo4jCsvOptions`](super::Neo4jCsvOptions). Quoting/escaping follows
/// RFC 4180 (doubled quotes escape a literal quote); the array delimiter is
/// applied later, in the Neo4j coercion layer, not here.
pub(crate) fn csv_rows_with(path: &Path, delimiter: u8, quote: u8) -> Result<RowIter, ImportError> {
    let file = File::open(path)
        .map_err(|e| ImportError::Io(format!("opening {}: {e}", path.display())))?;
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(true)
        .delimiter(delimiter)
        .quote(quote)
        .from_reader(file);

    let headers = reader
        .headers()
        .map_err(|e| ImportError::Io(format!("reading header of {}: {e}", path.display())))?
        .clone();

    let iter = reader.into_records().enumerate().map(move |(i, record)| {
        let row_num = i + 1;
        match record {
            Ok(record) => {
                let mut cells = HashMap::with_capacity(headers.len());
                for (header, value) in headers.iter().zip(record.iter()) {
                    cells.insert(header.to_string(), Cell::Str(value.to_string()));
                }
                Ok(Row {
                    index: row_num,
                    cells,
                })
            }
            Err(e) => Err(ImportError::Row(RowError {
                row: row_num,
                message: format!("CSV parse error: {e}"),
            })),
        }
    });

    Ok(Box::new(iter))
}

/// Read only the header row of a CSV file (Issue #3356).
///
/// The Neo4j importer builds its per-column coercion plan from the
/// self-describing header before streaming data rows via [`csv_rows_with`]
/// (whose reader skips the same header). Returns the ordered header fields.
pub(crate) fn read_csv_header(
    path: &Path,
    delimiter: u8,
    quote: u8,
) -> Result<Vec<String>, ImportError> {
    let file = File::open(path)
        .map_err(|e| ImportError::Io(format!("opening {}: {e}", path.display())))?;
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(true)
        .delimiter(delimiter)
        .quote(quote)
        .from_reader(file);
    let headers = reader
        .headers()
        .map_err(|e| ImportError::Io(format!("reading header of {}: {e}", path.display())))?;
    Ok(headers.iter().map(|h| h.to_string()).collect())
}

/// Build a streaming row reader over a JSONL file (one JSON object per line).
///
/// Blank lines are skipped and do not count as rows; the 1-based line number is used
/// as the row index for error reporting.
pub(crate) fn jsonl_rows(path: &Path) -> Result<RowIter, ImportError> {
    let file = File::open(path)
        .map_err(|e| ImportError::Io(format!("opening {}: {e}", path.display())))?;
    let reader = BufReader::new(file);

    let iter = reader.lines().enumerate().filter_map(move |(i, line)| {
        let line_num = i + 1;
        match line {
            Ok(line) => {
                if line.trim().is_empty() {
                    return None;
                }
                match serde_json::from_str::<serde_json::Value>(&line) {
                    Ok(serde_json::Value::Object(map)) => {
                        let cells = map
                            .into_iter()
                            .map(|(k, v)| (k, Cell::Json(v)))
                            .collect::<HashMap<_, _>>();
                        Some(Ok(Row {
                            index: line_num,
                            cells,
                        }))
                    }
                    Ok(_) => Some(Err(ImportError::Row(RowError {
                        row: line_num,
                        message: "JSONL line is not a JSON object".to_string(),
                    }))),
                    Err(e) => Some(Err(ImportError::Row(RowError {
                        row: line_num,
                        message: format!("invalid JSON: {e}"),
                    }))),
                }
            }
            Err(e) => Some(Err(ImportError::Row(RowError {
                row: line_num,
                message: format!("IO error reading line: {e}"),
            }))),
        }
    });

    Ok(Box::new(iter))
}
