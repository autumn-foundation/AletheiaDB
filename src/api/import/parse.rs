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
                Ok(Row::new(row_num, cells))
            }
            Err(e) => Err(ImportError::Row(RowError::new(
                row_num,
                format!("CSV parse error: {e}"),
            ))),
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
                        Some(Ok(Row::new(line_num, cells)))
                    }
                    Ok(_) => Some(Err(ImportError::Row(RowError::new(
                        line_num,
                        "JSONL line is not a JSON object".to_string(),
                    )))),
                    Err(e) => Some(Err(ImportError::Row(RowError::new(
                        line_num,
                        format!("invalid JSON: {e}"),
                    )))),
                }
            }
            Err(e) => Some(Err(ImportError::Row(RowError::new(
                line_num,
                format!("IO error reading line: {e}"),
            )))),
        }
    });

    Ok(Box::new(iter))
}

#[cfg(test)]
mod tests {
    //! Coercion / error-path coverage (Issue #3535 mutation backfill).
    //!
    //! These tests pin the exact `Err(...)` messages and `Ok(...)` values of the
    //! row-reader coercion helpers so the surviving mutants on this file's error
    //! branches (surfaced by the PR #3527 Mutants run) are killed: each test
    //! fails if the branch it targets is deleted or its return value swapped.

    use super::*;

    // --- get_str -----------------------------------------------------------

    #[test]
    fn get_str_returns_present_column_value() {
        let mut cells = HashMap::new();
        cells.insert("name".to_string(), Cell::Str("Alice".to_string()));
        let row = Row::new(1, cells);
        assert_eq!(row.get_str("name").unwrap(), "Alice");
    }

    #[test]
    fn get_str_missing_column_is_err_with_column_name() {
        let row = Row::new(1, HashMap::new());
        let err = row.get_str("nope").unwrap_err();
        assert_eq!(err, "missing column 'nope'");
    }

    // --- cell_to_string ----------------------------------------------------

    #[test]
    fn cell_to_string_covers_every_arm() {
        assert_eq!(cell_to_string(&Cell::Str("hi".to_string())), "hi");
        assert_eq!(
            cell_to_string(&Cell::Json(serde_json::Value::String("js".to_string()))),
            "js"
        );
        // JSON null renders as an empty string (treated as a blank cell).
        assert_eq!(cell_to_string(&Cell::Json(serde_json::Value::Null)), "");
        // A non-string JSON value renders via its JSON `to_string`.
        assert_eq!(
            cell_to_string(&Cell::Json(serde_json::json!(5))),
            "5".to_string()
        );
        assert_eq!(
            cell_to_string(&Cell::Json(serde_json::json!(true))),
            "true".to_string()
        );
    }

    // --- coerce_str error + success arms -----------------------------------

    #[test]
    fn coerce_str_string_preserves_raw_untrimmed() {
        // The String arm uses `raw`, not the trimmed value.
        assert_eq!(
            coerce_str(" pad ", ColumnType::String).unwrap(),
            PropertyValue::string(" pad ")
        );
    }

    #[test]
    fn coerce_str_int_ok_empty_and_error() {
        assert_eq!(
            coerce_str("42", ColumnType::Int).unwrap(),
            PropertyValue::Int(42)
        );
        assert_eq!(
            coerce_str("   ", ColumnType::Int).unwrap(),
            PropertyValue::Null
        );
        assert_eq!(
            coerce_str("x", ColumnType::Int).unwrap_err(),
            "cannot coerce 'x' to Int"
        );
    }

    #[test]
    fn coerce_str_float_ok_empty_and_error() {
        assert_eq!(
            coerce_str("1.5", ColumnType::Float).unwrap(),
            PropertyValue::Float(1.5)
        );
        assert_eq!(
            coerce_str("", ColumnType::Float).unwrap(),
            PropertyValue::Null
        );
        assert_eq!(
            coerce_str("x", ColumnType::Float).unwrap_err(),
            "cannot coerce 'x' to Float"
        );
    }

    #[test]
    fn coerce_str_bool_ok_empty_and_error() {
        assert_eq!(
            coerce_str("true", ColumnType::Bool).unwrap(),
            PropertyValue::Bool(true)
        );
        assert_eq!(
            coerce_str("", ColumnType::Bool).unwrap(),
            PropertyValue::Null
        );
        assert_eq!(
            coerce_str("maybe", ColumnType::Bool).unwrap_err(),
            "cannot coerce 'maybe' to Bool"
        );
    }

    #[test]
    fn coerce_str_timestamp_ok_empty_and_parse_failure() {
        assert_eq!(
            coerce_str("1000", ColumnType::Timestamp).unwrap(),
            PropertyValue::Int(1000)
        );
        assert_eq!(
            coerce_str("", ColumnType::Timestamp).unwrap(),
            PropertyValue::Null
        );
        // A non-empty, unparseable value must propagate the parse_valid_time
        // error (the Timestamp arm has no error message of its own).
        assert_eq!(
            coerce_str("garbage", ColumnType::Timestamp).unwrap_err(),
            "could not parse valid_time 'garbage'"
        );
    }

    #[test]
    fn coerce_str_embedding_ok_empty_and_non_json_error() {
        assert_eq!(
            coerce_str("[1, 2, 3]", ColumnType::Embedding).unwrap(),
            PropertyValue::try_vector([1.0f32, 2.0, 3.0]).unwrap()
        );
        assert_eq!(
            coerce_str("", ColumnType::Embedding).unwrap(),
            PropertyValue::Null
        );
        assert_eq!(
            coerce_str("not-json", ColumnType::Embedding).unwrap_err(),
            "cannot coerce 'not-json' to Embedding (expected JSON array)"
        );
    }

    // --- json_to_embedding -------------------------------------------------

    #[test]
    fn json_to_embedding_non_array_is_err() {
        assert_eq!(
            json_to_embedding(&serde_json::json!({"a": 1})).unwrap_err(),
            "cannot coerce non-array JSON to Embedding"
        );
        assert_eq!(
            json_to_embedding(&serde_json::json!(7)).unwrap_err(),
            "cannot coerce non-array JSON to Embedding"
        );
    }

    #[test]
    fn json_to_embedding_non_number_element_is_err() {
        assert_eq!(
            json_to_embedding(&serde_json::json!([1, "x", 3])).unwrap_err(),
            "embedding element \"x\" is not a number"
        );
    }

    #[test]
    fn json_to_embedding_valid_array_builds_vector() {
        assert_eq!(
            json_to_embedding(&serde_json::json!([1.0, 2.0, 3.0])).unwrap(),
            PropertyValue::try_vector([1.0f32, 2.0, 3.0]).unwrap()
        );
    }

    #[test]
    fn json_to_embedding_try_vector_failure_propagates() {
        // A dimension over MAX_VECTOR_DIMENSIONS makes `try_vector` fail; the
        // `.map_err(|e| e.to_string())` arm must surface that as a non-empty
        // error string (not an Ok, not an empty string).
        use crate::core::vector::constants::MAX_VECTOR_DIMENSIONS;
        let oversized =
            serde_json::Value::Array(vec![serde_json::json!(0.0); MAX_VECTOR_DIMENSIONS + 1]);
        let err = json_to_embedding(&oversized).unwrap_err();
        assert!(!err.is_empty(), "try_vector error must not be empty");
    }

    // --- coerce_json error + success arms ----------------------------------

    #[test]
    fn coerce_json_null_is_null_for_any_type() {
        assert_eq!(
            coerce_json(&serde_json::Value::Null, ColumnType::Int).unwrap(),
            PropertyValue::Null
        );
        assert_eq!(
            coerce_json(&serde_json::Value::Null, ColumnType::Embedding).unwrap(),
            PropertyValue::Null
        );
    }

    #[test]
    fn coerce_json_string_arms() {
        assert_eq!(
            coerce_json(&serde_json::json!("hi"), ColumnType::String).unwrap(),
            PropertyValue::string("hi")
        );
        // Non-string JSON coerced to a String column stringifies.
        assert_eq!(
            coerce_json(&serde_json::json!(5), ColumnType::String).unwrap(),
            PropertyValue::string("5")
        );
    }

    #[test]
    fn coerce_json_int_number_non_integer_is_err() {
        // 1.5 has no i64 representation -> the `as_i64` error arm fires.
        assert_eq!(
            coerce_json(&serde_json::json!(1.5), ColumnType::Int).unwrap_err(),
            "cannot coerce '1.5' to Int"
        );
        assert_eq!(
            coerce_json(&serde_json::json!(9), ColumnType::Int).unwrap(),
            PropertyValue::Int(9)
        );
    }

    #[test]
    fn coerce_json_int_string_delegates() {
        assert_eq!(
            coerce_json(&serde_json::json!("7"), ColumnType::Int).unwrap(),
            PropertyValue::Int(7)
        );
        assert_eq!(
            coerce_json(&serde_json::json!("x"), ColumnType::Int).unwrap_err(),
            "cannot coerce 'x' to Int"
        );
    }

    #[test]
    fn coerce_json_float_and_bool_arms() {
        assert_eq!(
            coerce_json(&serde_json::json!(2.5), ColumnType::Float).unwrap(),
            PropertyValue::Float(2.5)
        );
        assert_eq!(
            coerce_json(&serde_json::json!("3.5"), ColumnType::Float).unwrap(),
            PropertyValue::Float(3.5)
        );
        assert_eq!(
            coerce_json(&serde_json::json!(true), ColumnType::Bool).unwrap(),
            PropertyValue::Bool(true)
        );
        assert_eq!(
            coerce_json(&serde_json::json!("false"), ColumnType::Bool).unwrap(),
            PropertyValue::Bool(false)
        );
    }

    #[test]
    fn coerce_json_timestamp_number_non_integer_is_err() {
        assert_eq!(
            coerce_json(&serde_json::json!(1.25), ColumnType::Timestamp).unwrap_err(),
            "cannot coerce '1.25' to Timestamp"
        );
        assert_eq!(
            coerce_json(&serde_json::json!(1000), ColumnType::Timestamp).unwrap(),
            PropertyValue::Int(1000)
        );
    }

    #[test]
    fn coerce_json_timestamp_string_delegates() {
        // The (Timestamp, String) arm routes through coerce_str.
        assert_eq!(
            coerce_json(&serde_json::json!("2000"), ColumnType::Timestamp).unwrap(),
            PropertyValue::Int(2000)
        );
        assert_eq!(
            coerce_json(&serde_json::json!("garbage"), ColumnType::Timestamp).unwrap_err(),
            "could not parse valid_time 'garbage'"
        );
    }

    #[test]
    fn coerce_json_embedding_arms() {
        assert_eq!(
            coerce_json(&serde_json::json!([1.0, 2.0, 3.0]), ColumnType::Embedding).unwrap(),
            PropertyValue::try_vector([1.0f32, 2.0, 3.0]).unwrap()
        );
        assert_eq!(
            coerce_json(&serde_json::json!("[4, 5, 6]"), ColumnType::Embedding).unwrap(),
            PropertyValue::try_vector([4.0f32, 5.0, 6.0]).unwrap()
        );
    }

    #[test]
    fn coerce_json_catch_all_type_mismatch_is_err() {
        // A JSON bool coerced to an Int column hits the final catch-all arm.
        assert_eq!(
            coerce_json(&serde_json::json!(true), ColumnType::Int).unwrap_err(),
            "cannot coerce JSON true to Int"
        );
    }

    #[test]
    fn coerce_dispatches_str_and_json() {
        assert_eq!(
            coerce(&Cell::Str("5".to_string()), ColumnType::Int).unwrap(),
            PropertyValue::Int(5)
        );
        assert_eq!(
            coerce(&Cell::Json(serde_json::json!(6)), ColumnType::Int).unwrap(),
            PropertyValue::Int(6)
        );
    }

    // --- parse_bool --------------------------------------------------------

    #[test]
    fn parse_bool_truthy_tokens() {
        for t in ["true", "1", "yes", "t", "TRUE", "Yes"] {
            assert_eq!(parse_bool(t), Some(true), "token {t}");
        }
    }

    #[test]
    fn parse_bool_falsy_tokens() {
        for f in ["false", "0", "no", "f", "FALSE", "No"] {
            assert_eq!(parse_bool(f), Some(false), "token {f}");
        }
    }

    #[test]
    fn parse_bool_unknown_is_none() {
        assert_eq!(parse_bool("maybe"), None);
        assert_eq!(parse_bool("2"), None);
        assert_eq!(parse_bool(""), None);
    }

    // --- parse_valid_time --------------------------------------------------

    #[test]
    fn parse_valid_time_empty_is_err() {
        assert_eq!(
            parse_valid_time("   ").unwrap_err(),
            "empty valid_time value"
        );
    }

    #[test]
    fn parse_valid_time_bare_integer_micros() {
        assert_eq!(parse_valid_time("1000000").unwrap().wallclock(), 1_000_000);
    }

    // 2024-01-15T00:00:00Z is exactly 1_705_276_800 s = 1_705_276_800_000_000 µs
    // since the Unix epoch. Pinning the absolute value catches a uniform
    // conversion-corrupting mutant that a relative (x == Z-form) check misses.
    const JAN_15_2024_UTC_MICROS: i64 = 1_705_276_800_000_000;

    #[test]
    fn parse_valid_time_rfc3339_utc_and_offset() {
        let z = parse_valid_time("2024-01-15T00:00:00Z").unwrap();
        let off = parse_valid_time("2024-01-15T02:00:00+02:00").unwrap();
        // Absolute instant (kills uniform-offset mutants), and the same instant
        // expressed two ways -> identical micros.
        assert_eq!(z.wallclock(), JAN_15_2024_UTC_MICROS);
        assert_eq!(off.wallclock(), JAN_15_2024_UTC_MICROS);
    }

    #[test]
    fn parse_valid_time_naive_datetime_assumed_utc() {
        let naive = parse_valid_time("2024-01-15T00:00:00").unwrap();
        assert_eq!(naive.wallclock(), JAN_15_2024_UTC_MICROS);
    }

    #[test]
    fn parse_valid_time_bare_date_is_midnight_utc() {
        let date = parse_valid_time("2024-01-15").unwrap();
        assert_eq!(date.wallclock(), JAN_15_2024_UTC_MICROS);
    }

    #[test]
    fn parse_valid_time_garbage_is_err() {
        assert_eq!(
            parse_valid_time("not-a-date").unwrap_err(),
            "could not parse valid_time 'not-a-date'"
        );
    }

    // --- parquet-only helpers ---------------------------------------------

    #[cfg(feature = "parquet")]
    #[test]
    fn property_value_to_string_covers_every_arm() {
        assert_eq!(property_value_to_string(&PropertyValue::Null), "");
        assert_eq!(property_value_to_string(&PropertyValue::Bool(true)), "true");
        assert_eq!(property_value_to_string(&PropertyValue::Int(7)), "7");
        assert_eq!(property_value_to_string(&PropertyValue::Float(1.5)), "1.5");
        assert_eq!(property_value_to_string(&PropertyValue::string("hi")), "hi");
        // The catch-all arm Debug-formats (e.g. a Vector).
        let vector = PropertyValue::try_vector([1.0f32, 2.0, 3.0]).unwrap();
        let rendered = property_value_to_string(&vector);
        assert!(
            rendered.contains("Vector"),
            "expected Debug of a Vector, got {rendered}"
        );
    }

    #[cfg(feature = "parquet")]
    #[test]
    fn cell_to_string_native_arm() {
        assert_eq!(cell_to_string(&Cell::Native(PropertyValue::Int(9))), "9");
        assert_eq!(cell_to_string(&Cell::Native(PropertyValue::Null)), "");
    }

    #[cfg(feature = "parquet")]
    #[test]
    fn coerce_native_null_is_null() {
        assert_eq!(
            coerce_native(&PropertyValue::Null, ColumnType::Int).unwrap(),
            PropertyValue::Null
        );
    }

    #[cfg(feature = "parquet")]
    #[test]
    fn coerce_native_string_arms() {
        assert_eq!(
            coerce_native(&PropertyValue::string("hi"), ColumnType::String).unwrap(),
            PropertyValue::string("hi")
        );
        // A non-string widens via property_value_to_string.
        assert_eq!(
            coerce_native(&PropertyValue::Int(4), ColumnType::String).unwrap(),
            PropertyValue::string("4")
        );
    }

    #[cfg(feature = "parquet")]
    #[test]
    fn coerce_native_int_arms_and_error() {
        assert_eq!(
            coerce_native(&PropertyValue::Int(3), ColumnType::Int).unwrap(),
            PropertyValue::Int(3)
        );
        assert_eq!(
            coerce_native(&PropertyValue::string("7"), ColumnType::Int).unwrap(),
            PropertyValue::Int(7)
        );
        assert_eq!(
            coerce_native(&PropertyValue::Bool(true), ColumnType::Int).unwrap_err(),
            "cannot coerce bool to Int"
        );
    }

    #[cfg(feature = "parquet")]
    #[test]
    fn coerce_native_float_arms_and_error() {
        assert_eq!(
            coerce_native(&PropertyValue::Float(2.5), ColumnType::Float).unwrap(),
            PropertyValue::Float(2.5)
        );
        // Int widens to Float.
        assert_eq!(
            coerce_native(&PropertyValue::Int(4), ColumnType::Float).unwrap(),
            PropertyValue::Float(4.0)
        );
        assert_eq!(
            coerce_native(&PropertyValue::string("1.5"), ColumnType::Float).unwrap(),
            PropertyValue::Float(1.5)
        );
        assert_eq!(
            coerce_native(&PropertyValue::Bool(false), ColumnType::Float).unwrap_err(),
            "cannot coerce bool to Float"
        );
    }

    #[cfg(feature = "parquet")]
    #[test]
    fn coerce_native_bool_arms_and_error() {
        assert_eq!(
            coerce_native(&PropertyValue::Bool(true), ColumnType::Bool).unwrap(),
            PropertyValue::Bool(true)
        );
        assert_eq!(
            coerce_native(&PropertyValue::string("no"), ColumnType::Bool).unwrap(),
            PropertyValue::Bool(false)
        );
        assert_eq!(
            coerce_native(&PropertyValue::Int(1), ColumnType::Bool).unwrap_err(),
            "cannot coerce int to Bool"
        );
    }

    #[cfg(feature = "parquet")]
    #[test]
    fn coerce_native_timestamp_arms_and_error() {
        assert_eq!(
            coerce_native(&PropertyValue::Int(1000), ColumnType::Timestamp).unwrap(),
            PropertyValue::Int(1000)
        );
        assert_eq!(
            coerce_native(&PropertyValue::string("2000"), ColumnType::Timestamp).unwrap(),
            PropertyValue::Int(2000)
        );
        assert_eq!(
            coerce_native(&PropertyValue::Bool(true), ColumnType::Timestamp).unwrap_err(),
            "cannot coerce bool to Timestamp"
        );
    }

    #[cfg(feature = "parquet")]
    #[test]
    fn coerce_native_embedding_arms_and_error() {
        let vector = PropertyValue::try_vector([1.0f32, 2.0, 3.0]).unwrap();
        assert_eq!(
            coerce_native(&vector, ColumnType::Embedding).unwrap(),
            vector
        );
        assert_eq!(
            coerce_native(&PropertyValue::Int(1), ColumnType::Embedding).unwrap_err(),
            "cannot coerce int to Embedding"
        );
    }
}
