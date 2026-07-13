//! Streaming Parquet row reader for the bulk importer (Issue #3364).
//!
//! Produces the same `Result<Row, ImportError>` stream the CSV/JSONL readers in
//! [`super::parse`] yield, so the format-agnostic import loop
//! ([`Importer::import_nodes`](super::Importer) / `import_edges`) is reused verbatim
//! — inheriting the #3211 abort/skip semantics, precise `row N:` errors,
//! chunk-boundary partial-commit, null-as-absent, and per-row `valid_time` backfill.
//!
//! Columns are decoded **physically** from their Arrow array to a native
//! [`PropertyValue`] (numbers, booleans, timestamps, and `list<float32>` embeddings
//! never round-trip through a string), wrapped in [`Cell::Native`]. A SQL null cell
//! decodes to [`PropertyValue::Null`], which the property builder treats as an absent
//! property — matching the CSV blank-cell contract.
//!
//! Reading is chunked in Arrow `RecordBatch`es of [`PARQUET_BATCH_SIZE`] rows, so a
//! multi-row-group file is streamed with bounded memory rather than fully
//! materialized. A stable 1-based logical row index runs across every row group, so
//! `row N:` error messages stay precise.

use std::collections::HashMap;
use std::fs::File;
use std::path::Path;

use arrow::array::{
    Array, ArrayRef, BinaryArray, BooleanArray, Date32Array, Date64Array, Float32Array,
    Float64Array, Int8Array, Int16Array, Int32Array, Int64Array, LargeBinaryArray,
    LargeStringArray, RecordBatch, StringArray, TimestampMicrosecondArray,
    TimestampMillisecondArray, TimestampNanosecondArray, TimestampSecondArray, UInt8Array,
    UInt16Array, UInt32Array, UInt64Array,
};
use arrow::array::{GenericListArray, OffsetSizeTrait};
use arrow::datatypes::{DataType, TimeUnit};
use parquet::arrow::arrow_reader::ParquetRecordBatchReader;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

use super::parse::{Cell, Row, RowIter};
use super::{ImportError, RowError};
use crate::core::property::PropertyValue;

/// Rows per Arrow `RecordBatch` decoded from the Parquet file. Bounds peak import
/// memory to roughly one batch of decoded cells rather than the whole file.
const PARQUET_BATCH_SIZE: usize = 8192;

/// Microseconds in one day, used to widen `Date32` (days since epoch) to micros.
const MICROS_PER_DAY: i64 = 86_400_000_000;

/// File-metadata key stamped by the AletheiaDB exporter. Its presence marks a file as
/// our own export, enabling auto-expansion of the `properties_json` overflow column.
const EXPORT_SCHEMA_VERSION_KEY: &str = "aletheiadb.schema_version";

/// The overflow column name written by the exporter (bytes / heterogeneous arrays /
/// sparse vectors / type-conflicting keys, as reversible tagged JSON).
const OVERFLOW_COLUMN: &str = "properties_json";

/// Build a streaming row reader over a Parquet file.
///
/// The file is opened up front (open / metadata-parse failure is fatal, matching the
/// CSV/JSONL readers); rows are then decoded lazily one `RecordBatch` at a time.
pub(crate) fn parquet_rows(path: &Path) -> Result<RowIter, ImportError> {
    let file = File::open(path)
        .map_err(|e| ImportError::Io(format!("opening {}: {e}", path.display())))?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)
        .map_err(|e| ImportError::Io(format!("opening parquet {}: {e}", path.display())))?;
    let schema = builder.schema();
    let field_names: Vec<String> = schema.fields().iter().map(|f| f.name().clone()).collect();
    // Auto-expand the overflow column only for our own exports (identified by the
    // schema-version file metadata) that actually carry a `properties_json` column, so a
    // foreign file with a same-named column is never reinterpreted.
    let expand_overflow = schema.metadata().contains_key(EXPORT_SCHEMA_VERSION_KEY)
        && field_names.iter().any(|n| n == OVERFLOW_COLUMN);
    let reader = builder
        .with_batch_size(PARQUET_BATCH_SIZE)
        .build()
        .map_err(|e| ImportError::Io(format!("reading parquet {}: {e}", path.display())))?;

    Ok(Box::new(ParquetRowIter {
        reader,
        field_names,
        expand_overflow,
        batch: None,
        row_in_batch: 0,
        logical_row: 0,
        stopped: false,
    }))
}

/// A streaming iterator over the rows of a Parquet file, one `RecordBatch` at a time.
struct ParquetRowIter {
    reader: ParquetRecordBatchReader,
    /// Column names in schema order; the cell map is keyed by these.
    field_names: Vec<String>,
    /// Whether to auto-expand the `properties_json` overflow column into native
    /// properties (true only for an AletheiaDB export carrying that column).
    expand_overflow: bool,
    /// The batch currently being drained, if any.
    batch: Option<RecordBatch>,
    /// Next row to emit within `batch`.
    row_in_batch: usize,
    /// 1-based logical row number across every row group (for `row N:` errors).
    logical_row: usize,
    /// Set once a fatal reader error has been surfaced, to stop iteration.
    stopped: bool,
}

impl Iterator for ParquetRowIter {
    type Item = Result<Row, ImportError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.stopped {
            return None;
        }

        // Advance to a batch that still has rows to emit, pulling new batches as needed.
        loop {
            let needs_batch = match &self.batch {
                Some(batch) => self.row_in_batch >= batch.num_rows(),
                None => true,
            };
            if needs_batch {
                match self.reader.next() {
                    None => return None,
                    Some(Ok(batch)) => {
                        if batch.num_rows() == 0 {
                            // Empty batch (possible for an empty row group): skip it.
                            self.batch = None;
                            continue;
                        }
                        self.batch = Some(batch);
                        self.row_in_batch = 0;
                    }
                    Some(Err(e)) => {
                        // A mid-stream reader error is fatal even in skip mode.
                        self.stopped = true;
                        return Some(Err(ImportError::Io(format!("reading parquet batch: {e}"))));
                    }
                }
            } else {
                break;
            }
        }

        let batch = self.batch.as_ref()?;
        let i = self.row_in_batch;
        self.row_in_batch += 1;
        self.logical_row += 1;
        let row_num = self.logical_row;

        let mut cells = HashMap::with_capacity(self.field_names.len());
        for (col_idx, name) in self.field_names.iter().enumerate() {
            let array = batch.column(col_idx);
            match decode_value(array.as_ref(), i) {
                Ok(value) => {
                    cells.insert(name.clone(), Cell::Native(value));
                }
                Err(message) => {
                    return Some(Err(ImportError::Row(RowError::new(
                        row_num,
                        format!("column '{name}': {message}"),
                    ))));
                }
            }
        }

        let mut row = Row::new(row_num, cells);

        // Auto-expand the overflow column of an AletheiaDB export back into native
        // properties, so callers need not map the exotic keys explicitly.
        if self.expand_overflow {
            match decode_overflow(row.cells.get(OVERFLOW_COLUMN)) {
                Ok(overflow) => row.overflow = overflow,
                Err(message) => {
                    return Some(Err(ImportError::Row(RowError::new(
                        row_num,
                        format!("column '{OVERFLOW_COLUMN}': {message}"),
                    ))));
                }
            }
        }

        Some(Ok(row))
    }
}

/// Decode the `properties_json` overflow cell of an AletheiaDB export into native
/// key → [`PropertyValue`] pairs (the inverse of the exporter's tagged-JSON encoding).
///
/// An absent or SQL-null cell yields no properties. A malformed cell (non-string, or a
/// string that is not a JSON object of tagged values) is a clean decode error carried up
/// as a `row N:` [`RowError`], never a panic.
fn decode_overflow(cell: Option<&Cell>) -> Result<Vec<(String, PropertyValue)>, String> {
    let json = match cell {
        None | Some(Cell::Native(PropertyValue::Null)) => return Ok(Vec::new()),
        Some(Cell::Native(PropertyValue::String(s))) => s.to_string(),
        Some(_) => return Err("expected a JSON string overflow column".to_string()),
    };
    let value: serde_json::Value =
        serde_json::from_str(&json).map_err(|e| format!("invalid overflow JSON: {e}"))?;
    let obj = value
        .as_object()
        .ok_or_else(|| "overflow column must be a JSON object".to_string())?;
    let mut out = Vec::with_capacity(obj.len());
    for (key, tagged) in obj {
        let decoded = crate::api::export::overflow_json_to_value(tagged)?;
        out.push((key.clone(), decoded));
    }
    Ok(out)
}

/// Downcast an Arrow array to a concrete typed array, erroring on mismatch.
fn downcast<A: Array + 'static>(array: &dyn Array) -> Result<&A, String> {
    array
        .as_any()
        .downcast_ref::<A>()
        .ok_or_else(|| "internal error: unexpected Arrow array layout".to_string())
}

/// Physically decode the cell at `row` of an Arrow column into a [`PropertyValue`].
///
/// A null slot decodes to [`PropertyValue::Null`] (treated as an absent property by
/// the caller). Timestamps and dates widen to microseconds since the Unix epoch and
/// are stored as [`PropertyValue::Int`]; `list<float32>`/`list<float64>` become a
/// [`PropertyValue::Vector`] embedding.
fn decode_value(array: &dyn Array, row: usize) -> Result<PropertyValue, String> {
    if array.is_null(row) {
        return Ok(PropertyValue::Null);
    }
    match array.data_type() {
        DataType::Boolean => Ok(PropertyValue::Bool(
            downcast::<BooleanArray>(array)?.value(row),
        )),
        DataType::Int8 => Ok(PropertyValue::Int(
            downcast::<Int8Array>(array)?.value(row) as i64
        )),
        DataType::Int16 => Ok(PropertyValue::Int(
            downcast::<Int16Array>(array)?.value(row) as i64,
        )),
        DataType::Int32 => Ok(PropertyValue::Int(
            downcast::<Int32Array>(array)?.value(row) as i64,
        )),
        DataType::Int64 => Ok(PropertyValue::Int(
            downcast::<Int64Array>(array)?.value(row),
        )),
        DataType::UInt8 => Ok(PropertyValue::Int(
            downcast::<UInt8Array>(array)?.value(row) as i64,
        )),
        DataType::UInt16 => Ok(PropertyValue::Int(
            downcast::<UInt16Array>(array)?.value(row) as i64,
        )),
        DataType::UInt32 => Ok(PropertyValue::Int(
            downcast::<UInt32Array>(array)?.value(row) as i64,
        )),
        DataType::UInt64 => {
            let v = downcast::<UInt64Array>(array)?.value(row);
            // A UInt64 above i64::MAX cannot be represented losslessly; reject rather
            // than silently wrapping to a negative i64.
            let as_i64 =
                i64::try_from(v).map_err(|_| format!("UInt64 value {v} exceeds the i64 range"))?;
            Ok(PropertyValue::Int(as_i64))
        }
        DataType::Float32 => Ok(PropertyValue::Float(
            downcast::<Float32Array>(array)?.value(row) as f64,
        )),
        DataType::Float64 => Ok(PropertyValue::Float(
            downcast::<Float64Array>(array)?.value(row),
        )),
        DataType::Utf8 => Ok(PropertyValue::string(
            downcast::<StringArray>(array)?.value(row),
        )),
        DataType::LargeUtf8 => Ok(PropertyValue::string(
            downcast::<LargeStringArray>(array)?.value(row),
        )),
        DataType::Timestamp(unit, _) => {
            let micros = timestamp_to_micros(array, row, unit)?;
            Ok(PropertyValue::Int(micros))
        }
        DataType::Date32 => {
            let days = downcast::<Date32Array>(array)?.value(row) as i64;
            // A hostile far-future/past date must not panic (debug) or wrap (release).
            let micros = days.checked_mul(MICROS_PER_DAY).ok_or_else(|| {
                format!(
                    "date value {days} (days since epoch) out of representable microsecond range"
                )
            })?;
            Ok(PropertyValue::Int(micros))
        }
        DataType::Date64 => {
            let millis = downcast::<Date64Array>(array)?.value(row);
            let micros = millis.checked_mul(1_000).ok_or_else(|| {
                format!("date value {millis}ms out of representable microsecond range")
            })?;
            Ok(PropertyValue::Int(micros))
        }
        DataType::Binary => Ok(PropertyValue::bytes(
            downcast::<BinaryArray>(array)?.value(row),
        )),
        DataType::LargeBinary => Ok(PropertyValue::bytes(
            downcast::<LargeBinaryArray>(array)?.value(row),
        )),
        DataType::List(field) => decode_embedding::<i32>(array, row, field.data_type()),
        DataType::LargeList(field) => decode_embedding::<i64>(array, row, field.data_type()),
        other => Err(format!("unsupported Parquet column type {other}")),
    }
}

/// Convert a timestamp value at `row` to microseconds since the Unix epoch.
fn timestamp_to_micros(array: &dyn Array, row: usize, unit: &TimeUnit) -> Result<i64, String> {
    let micros = match unit {
        TimeUnit::Second => {
            let secs = downcast::<TimestampSecondArray>(array)?.value(row);
            secs.checked_mul(1_000_000).ok_or_else(|| {
                format!("timestamp value {secs}s out of representable microsecond range")
            })?
        }
        TimeUnit::Millisecond => {
            let millis = downcast::<TimestampMillisecondArray>(array)?.value(row);
            millis.checked_mul(1_000).ok_or_else(|| {
                format!("timestamp value {millis}ms out of representable microsecond range")
            })?
        }
        TimeUnit::Microsecond => downcast::<TimestampMicrosecondArray>(array)?.value(row),
        TimeUnit::Nanosecond => downcast::<TimestampNanosecondArray>(array)?.value(row) / 1_000,
    };
    Ok(micros)
}

/// Decode a list cell into a [`PropertyValue::Vector`] embedding, requiring a
/// float element type.
fn decode_embedding<O: OffsetSizeTrait>(
    array: &dyn Array,
    row: usize,
    element_type: &DataType,
) -> Result<PropertyValue, String> {
    match element_type {
        DataType::Float32 | DataType::Float64 => {}
        other => {
            return Err(format!(
                "unsupported list element type {other} (only float lists map to embeddings)"
            ));
        }
    }
    let list = downcast::<GenericListArray<O>>(array)?;
    let values: ArrayRef = list.value(row);
    let len = values.len();
    let mut floats = Vec::with_capacity(len);
    if let Some(f32s) = values.as_any().downcast_ref::<Float32Array>() {
        for i in 0..len {
            if f32s.is_null(i) {
                return Err("embedding contains null element".to_string());
            }
            floats.push(f32s.value(i));
        }
    } else if let Some(f64s) = values.as_any().downcast_ref::<Float64Array>() {
        for i in 0..len {
            if f64s.is_null(i) {
                return Err("embedding contains null element".to_string());
            }
            floats.push(f64s.value(i) as f32);
        }
    } else {
        return Err("unsupported embedding element layout".to_string());
    }
    PropertyValue::try_vector(&floats).map_err(|e| e.to_string())
}
