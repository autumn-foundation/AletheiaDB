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
/// preserve their parsed JSON type; Parquet cells arrive already decoded to a
/// native [`PropertyValue`] (physical decode from the Arrow array), so numeric,
/// boolean, timestamp, and embedding columns never round-trip through a string.
#[derive(Debug, Clone)]
pub(crate) enum Cell {
    Str(String),
    Json(serde_json::Value),
    /// A Parquet cell physically decoded from its Arrow array. `Null` here means
    /// the column value was absent (SQL null) for this row.
    #[cfg(feature = "parquet")]
    Native(PropertyValue),
}

/// One parsed input row: the 1-based row/line number plus its named cells.
#[derive(Debug, Clone, Default)]
pub(crate) struct Row {
    pub(crate) index: usize,
    pub(crate) cells: HashMap<String, Cell>,
    /// Already-decoded properties injected into the row independently of the caller's
    /// column mapping. Populated only by the Parquet reader when it auto-expands the
    /// `properties_json` overflow column of an AletheiaDB export (Issue #3364); always
    /// empty for CSV/JSONL. Merged into the built [`PropertyMap`] by
    /// [`super::build_properties`], with explicit mapping columns taking precedence.
    #[cfg(feature = "parquet")]
    pub(crate) overflow: Vec<(String, PropertyValue)>,
}

/// A boxed, streaming iterator of parsed rows.
pub(crate) type RowIter = Box<dyn Iterator<Item = Result<Row, ImportError>>>;

impl Row {
    /// Construct a row from its 1-based index and named cells (no overflow properties).
    pub(crate) fn new(index: usize, cells: HashMap<String, Cell>) -> Self {
        Row {
            index,
            cells,
            #[cfg(feature = "parquet")]
            overflow: Vec::new(),
        }
    }

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
        #[cfg(feature = "parquet")]
        Cell::Native(value) => property_value_to_string(value),
    }
}

/// Render a physically-decoded Parquet [`PropertyValue`] as a plain string for the
/// label / business-key / valid-time paths. `Null` renders empty (so a null cell is
/// treated exactly like a blank CSV cell); an integer timestamp renders as its
/// microseconds, which `parse_valid_time` re-reads losslessly.
#[cfg(feature = "parquet")]
fn property_value_to_string(value: &PropertyValue) -> String {
    match value {
        PropertyValue::Null => String::new(),
        PropertyValue::Bool(b) => b.to_string(),
        PropertyValue::Int(i) => i.to_string(),
        PropertyValue::Float(f) => f.to_string(),
        PropertyValue::String(s) => s.to_string(),
        other => format!("{other:?}"),
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
        #[cfg(feature = "parquet")]
        Cell::Native(value) => coerce_native(value, ty),
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
        ColumnType::Timestamp => {
            if trimmed.is_empty() {
                return Ok(PropertyValue::Null);
            }
            parse_valid_time(trimmed).map(|ts| PropertyValue::Int(ts.wallclock()))
        }
        ColumnType::Embedding => {
            if trimmed.is_empty() {
                return Ok(PropertyValue::Null);
            }
            let json: serde_json::Value = serde_json::from_str(trimmed)
                .map_err(|_| format!("cannot coerce '{raw}' to Embedding (expected JSON array)"))?;
            json_to_embedding(&json)
        }
    }
}

/// Convert a JSON array of numbers into a [`PropertyValue::Vector`].
fn json_to_embedding(value: &serde_json::Value) -> Result<PropertyValue, String> {
    let serde_json::Value::Array(items) = value else {
        return Err("cannot coerce non-array JSON to Embedding".to_string());
    };
    let mut floats = Vec::with_capacity(items.len());
    for item in items {
        let f = item
            .as_f64()
            .ok_or_else(|| format!("embedding element {item} is not a number"))?;
        floats.push(f as f32);
    }
    PropertyValue::try_vector(&floats).map_err(|e| e.to_string())
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
        (ColumnType::Timestamp, Value::Number(n)) => n
            .as_i64()
            .map(PropertyValue::Int)
            .ok_or_else(|| format!("cannot coerce '{n}' to Timestamp")),
        (ColumnType::Timestamp, Value::String(s)) => coerce_str(s, ColumnType::Timestamp),
        (ColumnType::Embedding, arr @ Value::Array(_)) => json_to_embedding(arr),
        (ColumnType::Embedding, Value::String(s)) => coerce_str(s, ColumnType::Embedding),
        (ty, other) => Err(format!("cannot coerce JSON {other} to {ty:?}")),
    }
}

/// Coerce a physically-decoded Parquet [`PropertyValue`] to the mapping's requested
/// [`ColumnType`], widening/reconciling where the physical and logical types differ.
#[cfg(feature = "parquet")]
fn coerce_native(value: &PropertyValue, ty: ColumnType) -> Result<PropertyValue, String> {
    // A null column value is "absent" for every target type.
    if matches!(value, PropertyValue::Null) {
        return Ok(PropertyValue::Null);
    }
    match ty {
        ColumnType::String => match value {
            PropertyValue::String(_) => Ok(value.clone()),
            other => Ok(PropertyValue::string(property_value_to_string(other))),
        },
        ColumnType::Int => match value {
            PropertyValue::Int(_) => Ok(value.clone()),
            PropertyValue::String(s) => coerce_str(s, ColumnType::Int),
            other => Err(format!("cannot coerce {} to Int", other.type_name())),
        },
        ColumnType::Float => match value {
            PropertyValue::Float(_) => Ok(value.clone()),
            PropertyValue::Int(i) => Ok(PropertyValue::Float(*i as f64)),
            PropertyValue::String(s) => coerce_str(s, ColumnType::Float),
            other => Err(format!("cannot coerce {} to Float", other.type_name())),
        },
        ColumnType::Bool => match value {
            PropertyValue::Bool(_) => Ok(value.clone()),
            PropertyValue::String(s) => coerce_str(s, ColumnType::Bool),
            other => Err(format!("cannot coerce {} to Bool", other.type_name())),
        },
        ColumnType::Timestamp => match value {
            PropertyValue::Int(_) => Ok(value.clone()),
            PropertyValue::String(s) => coerce_str(s, ColumnType::Timestamp),
            other => Err(format!("cannot coerce {} to Timestamp", other.type_name())),
        },
        ColumnType::Embedding => match value {
            PropertyValue::Vector(_) => Ok(value.clone()),
            other => Err(format!("cannot coerce {} to Embedding", other.type_name())),
        },
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
    let file = File::open(path)
        .map_err(|e| ImportError::Io(format!("opening {}: {e}", path.display())))?;
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(true)
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
                Ok(Row::new(row_num, cells))
            }
            Err(e) => Err(ImportError::Row(RowError {
                row: row_num,
                message: format!("CSV parse error: {e}"),
            })),
        }
    });

    Ok(Box::new(iter))
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
                        Some(Ok(Row::new(line_num, cells)))
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
