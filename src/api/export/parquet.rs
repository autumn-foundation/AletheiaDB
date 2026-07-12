//! Streaming Parquet writer for the graph exporter (Issue #3364).
//!
//! The property schema is dynamic (each node/edge carries its own keys), so a first
//! scan over the database discovers, per property key, a single native columnar type —
//! or routes the key to a `properties_json` overflow column when its type conflicts
//! across rows or has no natural columnar form (bytes, heterogeneous arrays, sparse
//! vectors). A second scan then streams rows into fixed-size Arrow `RecordBatch`es,
//! each written as one Parquet row group so peak memory stays bounded.
//!
//! The emitted column layout mirrors what the importer
//! ([`crate::api::import`]) expects, so an export re-imports losslessly for scalars,
//! timestamps, and `list<float32>` embeddings.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use arrow::array::{
    ArrayRef, BooleanArray, Float64Array, Int64Array, ListArray, RecordBatch, StringArray,
    TimestampMicrosecondArray,
};
use arrow::datatypes::{DataType, Field, Float32Type, Schema, TimeUnit};
use parquet::arrow::ArrowWriter;
use parquet::file::properties::WriterProperties;

use crate::core::changefeed::{ChangeFeedQuery, EntityKind};
use crate::core::error::{Error, Result};
use crate::core::history::VersionInfo;
use crate::core::id::{EdgeId, NodeId};
use crate::core::interning::GLOBAL_INTERNER;
use crate::core::property::{PropertyMap, PropertyValue};
use crate::core::provenance::Provenance;
use crate::core::temporal::{TIMESTAMP_MAX, Timestamp};
use crate::db::AletheiaDB;

use super::ExportReport;

/// Schema version stamped into the file-level metadata.
const SCHEMA_VERSION: &str = "1";

// Fixed column names (the documented, stable schema).
const COL_ID: &str = "id";
const COL_LABEL: &str = "label";
const COL_VALID_TIME: &str = "valid_time";
const COL_EDGE_TYPE: &str = "edge_type";
const COL_SOURCE_KEY: &str = "source_key";
const COL_TARGET_KEY: &str = "target_key";
const COL_VERSION: &str = "version";
const COL_VALID_FROM: &str = "valid_from";
const COL_VALID_TO: &str = "valid_to";
const COL_TRANSACTION_TIME: &str = "transaction_time";
const COL_TOMBSTONE: &str = "tombstone";
const COL_PROV_SOURCE: &str = "provenance_source";
const COL_PROV_CONFIDENCE: &str = "provenance_confidence";
const COL_PROV_NOTE: &str = "provenance_note";
const COL_PROV_CORRELATION_ID: &str = "provenance_correlation_id";
const COL_PROV_PRINCIPAL: &str = "provenance_principal";
const COL_PROPERTIES_JSON: &str = "properties_json";

/// The native columnar type chosen for a property key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ColKind {
    Int,
    Float,
    Bool,
    Str,
    Embedding,
}

/// How a property key is routed in the exported schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    /// A dedicated native typed column.
    Native(ColKind),
    /// The reversible tagged-JSON `properties_json` overflow column.
    Overflow,
}

/// Classify a single value into its column route, or `None` for a null (absent) value.
fn classify(value: &PropertyValue) -> Option<Route> {
    match value {
        PropertyValue::Null => None,
        PropertyValue::Bool(_) => Some(Route::Native(ColKind::Bool)),
        PropertyValue::Int(_) => Some(Route::Native(ColKind::Int)),
        PropertyValue::Float(_) => Some(Route::Native(ColKind::Float)),
        PropertyValue::String(_) => Some(Route::Native(ColKind::Str)),
        PropertyValue::Vector(_) => Some(Route::Native(ColKind::Embedding)),
        // No natural single-column form: route to the JSON overflow column.
        PropertyValue::Bytes(_) | PropertyValue::Array(_) | PropertyValue::SparseVector(_) => {
            Some(Route::Overflow)
        }
    }
}

/// Merge a newly-observed route into the running decision for a key. Any conflict
/// between two native kinds, or the presence of an overflow value, makes the key an
/// overflow column (absorbing).
fn merge(existing: Route, incoming: Route) -> Route {
    match (existing, incoming) {
        (Route::Native(a), Route::Native(b)) if a == b => Route::Native(a),
        _ => Route::Overflow,
    }
}

/// Resolve an interned property key to its string name.
fn key_name(key: &crate::core::interning::InternedString) -> Option<String> {
    GLOBAL_INTERNER.resolve_with(*key, |s| s.to_string())
}

/// Accumulates the per-key routing decision over a discovery scan.
struct SchemaBuilder<'a> {
    routes: BTreeMap<String, Route>,
    reserved: &'a [&'a str],
}

impl<'a> SchemaBuilder<'a> {
    fn new(reserved: &'a [&'a str]) -> Self {
        SchemaBuilder {
            routes: BTreeMap::new(),
            reserved,
        }
    }

    fn observe(&mut self, props: &PropertyMap) {
        for (key, value) in props.iter() {
            let Some(name) = key_name(key) else { continue };
            // A key colliding with a fixed column name is always overflow-routed so
            // the emitted schema stays unambiguous.
            let incoming = if self.reserved.contains(&name.as_str()) {
                Route::Overflow
            } else {
                match classify(value) {
                    Some(route) => route,
                    None => continue,
                }
            };
            self.routes
                .entry(name)
                .and_modify(|r| *r = merge(*r, incoming))
                .or_insert(incoming);
        }
    }

    fn finish(self) -> PropSchema {
        let mut native = Vec::new();
        let mut overflow = Vec::new();
        for (name, route) in self.routes {
            match route {
                Route::Native(kind) => native.push((name, kind)),
                Route::Overflow => overflow.push(name),
            }
        }
        PropSchema { native, overflow }
    }
}

/// The resolved property schema: native typed columns (sorted by key) plus the set of
/// overflow keys (sorted).
struct PropSchema {
    native: Vec<(String, ColKind)>,
    overflow: Vec<String>,
}

impl PropSchema {
    fn has_overflow(&self) -> bool {
        !self.overflow.is_empty()
    }

    /// Arrow fields for the property columns, in emission order (native columns, then
    /// the `properties_json` overflow column when present).
    fn fields(&self) -> Vec<Field> {
        let mut fields = Vec::with_capacity(self.native.len() + 1);
        for (name, kind) in &self.native {
            fields.push(Field::new(name, col_kind_datatype(*kind), true));
        }
        if self.has_overflow() {
            fields.push(Field::new(COL_PROPERTIES_JSON, DataType::Utf8, true));
        }
        fields
    }
}

/// The Arrow `DataType` for a native column kind. Kept in sync with [`ColumnBuf`] so
/// the up-front schema matches the arrays produced at flush time.
fn col_kind_datatype(kind: ColKind) -> DataType {
    match kind {
        ColKind::Int => DataType::Int64,
        ColKind::Float => DataType::Float64,
        ColKind::Bool => DataType::Boolean,
        ColKind::Str => DataType::Utf8,
        ColKind::Embedding => DataType::List(Arc::new(Field::new("item", DataType::Float32, true))),
    }
}

/// A per-column value accumulator for one in-flight batch.
enum ColumnBuf {
    Int(Vec<Option<i64>>),
    Float(Vec<Option<f64>>),
    Bool(Vec<Option<bool>>),
    Str(Vec<Option<String>>),
    Embedding(Vec<Option<Vec<f32>>>),
}

impl ColumnBuf {
    fn new(kind: ColKind) -> Self {
        match kind {
            ColKind::Int => ColumnBuf::Int(Vec::new()),
            ColKind::Float => ColumnBuf::Float(Vec::new()),
            ColKind::Bool => ColumnBuf::Bool(Vec::new()),
            ColKind::Str => ColumnBuf::Str(Vec::new()),
            ColKind::Embedding => ColumnBuf::Embedding(Vec::new()),
        }
    }

    /// Push a value for this row. A missing value, or one whose type does not match the
    /// column (which discovery would have routed to overflow), becomes a null slot.
    fn push(&mut self, value: Option<&PropertyValue>) {
        match self {
            ColumnBuf::Int(v) => v.push(match value {
                Some(PropertyValue::Int(i)) => Some(*i),
                _ => None,
            }),
            ColumnBuf::Float(v) => v.push(match value {
                Some(PropertyValue::Float(f)) => Some(*f),
                _ => None,
            }),
            ColumnBuf::Bool(v) => v.push(match value {
                Some(PropertyValue::Bool(b)) => Some(*b),
                _ => None,
            }),
            ColumnBuf::Str(v) => v.push(match value {
                Some(PropertyValue::String(s)) => Some(s.to_string()),
                _ => None,
            }),
            ColumnBuf::Embedding(v) => v.push(match value {
                Some(PropertyValue::Vector(vec)) => Some(vec.to_vec()),
                _ => None,
            }),
        }
    }

    /// Build the Arrow array for the accumulated batch, draining the buffer.
    fn finish(&mut self) -> ArrayRef {
        match self {
            ColumnBuf::Int(v) => Arc::new(Int64Array::from(std::mem::take(v))),
            ColumnBuf::Float(v) => Arc::new(Float64Array::from(std::mem::take(v))),
            ColumnBuf::Bool(v) => Arc::new(BooleanArray::from(std::mem::take(v))),
            ColumnBuf::Str(v) => Arc::new(StringArray::from_iter(std::mem::take(v))),
            ColumnBuf::Embedding(v) => {
                let arr = ListArray::from_iter_primitive::<Float32Type, _, _>(
                    std::mem::take(v).into_iter().map(|opt| {
                        opt.map(|floats| floats.into_iter().map(Some).collect::<Vec<_>>())
                    }),
                );
                Arc::new(arr)
            }
        }
    }
}

/// Accumulates the property columns (native + JSON overflow) for one in-flight batch.
struct PropColumns {
    native: Vec<(String, ColumnBuf)>,
    overflow: Vec<String>,
    json: Vec<Option<String>>,
}

impl PropColumns {
    fn new(schema: &PropSchema) -> Self {
        PropColumns {
            native: schema
                .native
                .iter()
                .map(|(name, kind)| (name.clone(), ColumnBuf::new(*kind)))
                .collect(),
            overflow: schema.overflow.clone(),
            json: Vec::new(),
        }
    }

    fn has_overflow(&self) -> bool {
        !self.overflow.is_empty()
    }

    /// Push one row's properties into every column.
    fn push(&mut self, props: &PropertyMap) {
        for (name, buf) in &mut self.native {
            buf.push(props.get(name));
        }
        if self.has_overflow() {
            let mut obj = serde_json::Map::new();
            for key in &self.overflow {
                if let Some(value) = props.get(key)
                    && !matches!(value, PropertyValue::Null)
                {
                    obj.insert(key.clone(), overflow_value_to_json(value));
                }
            }
            if obj.is_empty() {
                self.json.push(None);
            } else {
                self.json
                    .push(Some(serde_json::Value::Object(obj).to_string()));
            }
        }
    }

    /// Append the finished property arrays (native columns, then the JSON overflow
    /// column when present) to `arrays`.
    fn finish_into(&mut self, arrays: &mut Vec<ArrayRef>) {
        for (_, buf) in &mut self.native {
            arrays.push(buf.finish());
        }
        if self.has_overflow() {
            arrays.push(Arc::new(StringArray::from_iter(std::mem::take(
                &mut self.json,
            ))));
        }
    }
}

/// Reversible tagged-JSON encoding of a property value routed to the overflow column.
///
/// The inverse is [`overflow_json_to_value`]; together they let bytes, heterogeneous
/// arrays, sparse vectors, and type-conflicting keys survive an export → decode round
/// trip losslessly even though the default importer does not auto-expand the column.
fn overflow_value_to_json(value: &PropertyValue) -> serde_json::Value {
    use serde_json::json;
    match value {
        PropertyValue::Null => serde_json::Value::Null,
        PropertyValue::Bool(b) => json!({ "t": "bool", "v": b }),
        PropertyValue::Int(i) => json!({ "t": "int", "v": i }),
        PropertyValue::Float(f) => json!({ "t": "float", "v": f }),
        PropertyValue::String(s) => json!({ "t": "str", "v": s.as_ref() }),
        PropertyValue::Bytes(b) => json!({ "t": "bytes", "v": b.as_ref() }),
        PropertyValue::Vector(v) => {
            json!({ "t": "vector", "v": v.iter().copied().collect::<Vec<f32>>() })
        }
        PropertyValue::Array(items) => json!({
            "t": "array",
            "v": items.iter().map(overflow_value_to_json).collect::<Vec<_>>(),
        }),
        PropertyValue::SparseVector(sv) => json!({
            "t": "sparse",
            "indices": sv.indices(),
            "values": sv.values(),
            "dim": sv.dimension(),
        }),
    }
}

/// Inverse of [`overflow_value_to_json`]. Used by tests (and available to custom
/// re-import tooling) to reconstruct the exact [`PropertyValue`] from its tagged JSON.
#[cfg(test)]
pub(super) fn overflow_json_to_value(
    value: &serde_json::Value,
) -> std::result::Result<PropertyValue, String> {
    let obj = value
        .as_object()
        .ok_or_else(|| "overflow value must be a JSON object".to_string())?;
    let tag = obj
        .get("t")
        .and_then(|t| t.as_str())
        .ok_or_else(|| "overflow value missing 't' tag".to_string())?;
    match tag {
        "bool" => Ok(PropertyValue::Bool(
            obj.get("v").and_then(|v| v.as_bool()).ok_or("bad bool")?,
        )),
        "int" => Ok(PropertyValue::Int(
            obj.get("v").and_then(|v| v.as_i64()).ok_or("bad int")?,
        )),
        "float" => Ok(PropertyValue::Float(
            obj.get("v").and_then(|v| v.as_f64()).ok_or("bad float")?,
        )),
        "str" => Ok(PropertyValue::string(
            obj.get("v").and_then(|v| v.as_str()).ok_or("bad str")?,
        )),
        "bytes" => {
            let arr = obj.get("v").and_then(|v| v.as_array()).ok_or("bad bytes")?;
            let bytes: Vec<u8> = arr
                .iter()
                .map(|n| n.as_u64().map(|u| u as u8).ok_or("bad byte"))
                .collect::<std::result::Result<_, _>>()?;
            Ok(PropertyValue::bytes(bytes))
        }
        "vector" => {
            let arr = obj
                .get("v")
                .and_then(|v| v.as_array())
                .ok_or("bad vector")?;
            let floats: Vec<f32> = arr
                .iter()
                .map(|n| n.as_f64().map(|f| f as f32).ok_or("bad float elem"))
                .collect::<std::result::Result<_, _>>()?;
            PropertyValue::try_vector(&floats).map_err(|e| e.to_string())
        }
        "array" => {
            let arr = obj.get("v").and_then(|v| v.as_array()).ok_or("bad array")?;
            let items: Vec<PropertyValue> = arr
                .iter()
                .map(overflow_json_to_value)
                .collect::<std::result::Result<_, _>>()?;
            Ok(PropertyValue::array(items))
        }
        "sparse" => {
            let indices: Vec<u32> = obj
                .get("indices")
                .and_then(|v| v.as_array())
                .ok_or("bad indices")?
                .iter()
                .map(|n| n.as_u64().map(|u| u as u32).ok_or("bad index"))
                .collect::<std::result::Result<_, _>>()?;
            let values: Vec<f32> = obj
                .get("values")
                .and_then(|v| v.as_array())
                .ok_or("bad values")?
                .iter()
                .map(|n| n.as_f64().map(|f| f as f32).ok_or("bad value"))
                .collect::<std::result::Result<_, _>>()?;
            let dim = obj.get("dim").and_then(|v| v.as_u64()).ok_or("bad dim")? as u32;
            let sv = crate::core::vector::SparseVec::new(indices, values, dim)
                .map_err(|e| e.to_string())?;
            Ok(PropertyValue::sparse_vector(sv))
        }
        other => Err(format!("unknown overflow tag '{other}'")),
    }
}

/// Build a UTC microsecond-timestamp array from optional epoch micros.
fn ts_array(values: Vec<Option<i64>>) -> ArrayRef {
    Arc::new(TimestampMicrosecondArray::from(values).with_timezone("UTC"))
}

/// The Arrow `DataType` of a UTC microsecond-timestamp column.
fn ts_datatype() -> DataType {
    DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into()))
}

/// File-level metadata describing the export.
fn file_metadata(mode: &str, entity: &str) -> HashMap<String, String> {
    let mut md = HashMap::new();
    md.insert(
        "aletheiadb.schema_version".to_string(),
        SCHEMA_VERSION.to_string(),
    );
    md.insert("aletheiadb.mode".to_string(), mode.to_string());
    md.insert("aletheiadb.entity".to_string(), entity.to_string());
    md
}

/// A streaming Parquet writer that flushes one row group per batch.
struct StreamWriter {
    schema: Arc<Schema>,
    writer: ArrowWriter<File>,
}

impl StreamWriter {
    fn create(path: &Path, schema: Arc<Schema>, batch_size: usize) -> Result<Self> {
        let file = File::create(path)
            .map_err(|e| Error::other(format!("creating {}: {e}", path.display())))?;
        let props = WriterProperties::builder()
            .set_max_row_group_row_count(Some(batch_size.max(1)))
            .build();
        let writer = ArrowWriter::try_new(file, schema.clone(), Some(props))
            .map_err(|e| Error::other(format!("opening parquet writer: {e}")))?;
        Ok(StreamWriter { schema, writer })
    }

    fn write(&mut self, arrays: Vec<ArrayRef>) -> Result<()> {
        let batch = RecordBatch::try_new(self.schema.clone(), arrays)
            .map_err(|e| Error::other(format!("building record batch: {e}")))?;
        self.writer
            .write(&batch)
            .map_err(|e| Error::other(format!("writing parquet batch: {e}")))
    }

    fn close(self) -> Result<()> {
        self.writer
            .close()
            .map_err(|e| Error::other(format!("closing parquet writer: {e}")))?;
        Ok(())
    }
}

// ============================================================================
// Current-state node export
// ============================================================================

/// Reserved fixed-column names for current-state node export.
const NODE_RESERVED: &[&str] = &[COL_ID, COL_LABEL, COL_VALID_TIME, COL_PROPERTIES_JSON];

pub(super) fn export_nodes(
    db: &AletheiaDB,
    path: &Path,
    batch_size: usize,
) -> Result<ExportReport> {
    let node_ids = db.get_all_node_ids();

    // Pass 1: discover the property schema.
    let mut builder = SchemaBuilder::new(NODE_RESERVED);
    for &id in &node_ids {
        if let Ok(node) = db.get_node(id) {
            builder.observe(&node.properties);
        }
    }
    let prop_schema = builder.finish();

    // Fixed columns, then property columns.
    let mut fields = vec![
        Field::new(COL_ID, DataType::Int64, false),
        Field::new(COL_LABEL, DataType::Utf8, false),
        Field::new(COL_VALID_TIME, ts_datatype(), true),
    ];
    fields.extend(prop_schema.fields());
    let schema = Arc::new(Schema::new_with_metadata(
        fields,
        file_metadata("current", "node"),
    ));

    let mut writer = StreamWriter::create(path, schema, batch_size)?;
    let mut report = ExportReport::default();

    // Pass 2: stream rows.
    let mut ids: Vec<i64> = Vec::new();
    let mut labels: Vec<Option<String>> = Vec::new();
    let mut valid_times: Vec<Option<i64>> = Vec::new();
    let mut props = PropColumns::new_from_prop_schema(&prop_schema);

    for &id in &node_ids {
        let Ok(node) = db.get_node(id) else { continue };
        ids.push(id.as_u64() as i64);
        labels.push(GLOBAL_INTERNER.resolve_with(node.label, |s| s.to_string()));
        valid_times.push(current_valid_from_node(db, node.current_version));
        props.push(&node.properties);
        report.nodes_exported += 1;

        if ids.len() >= batch_size {
            flush_current(
                &mut writer,
                &mut ids,
                &mut labels,
                &mut valid_times,
                &mut props,
            )?;
        }
    }
    if !ids.is_empty() {
        flush_current(
            &mut writer,
            &mut ids,
            &mut labels,
            &mut valid_times,
            &mut props,
        )?;
    }

    writer.close()?;
    Ok(report)
}

/// Flush the accumulated current-state node/edge shared tail (id-ish header handled by
/// caller-provided vecs) — used by the node current-state path.
fn flush_current(
    writer: &mut StreamWriter,
    ids: &mut Vec<i64>,
    labels: &mut Vec<Option<String>>,
    valid_times: &mut Vec<Option<i64>>,
    props: &mut PropColumns,
) -> Result<()> {
    let mut arrays: Vec<ArrayRef> = vec![
        Arc::new(Int64Array::from(std::mem::take(ids))),
        Arc::new(StringArray::from_iter(std::mem::take(labels))),
        ts_array(std::mem::take(valid_times)),
    ];
    props.finish_into(&mut arrays);
    writer.write(arrays)
}

// ============================================================================
// Current-state edge export
// ============================================================================

const EDGE_RESERVED: &[&str] = &[
    COL_EDGE_TYPE,
    COL_SOURCE_KEY,
    COL_TARGET_KEY,
    COL_VALID_TIME,
    COL_PROPERTIES_JSON,
];

pub(super) fn export_edges(
    db: &AletheiaDB,
    path: &Path,
    batch_size: usize,
) -> Result<ExportReport> {
    let node_ids = db.get_all_node_ids();

    // Pass 1: discover property schema over all edges (reached via outgoing adjacency).
    let mut builder = SchemaBuilder::new(EDGE_RESERVED);
    for &nid in &node_ids {
        for eid in db.get_outgoing_edges(nid) {
            if let Ok(edge) = db.get_edge(eid) {
                builder.observe(&edge.properties);
            }
        }
    }
    let prop_schema = builder.finish();

    let mut fields = vec![
        Field::new(COL_EDGE_TYPE, DataType::Utf8, false),
        Field::new(COL_SOURCE_KEY, DataType::Int64, false),
        Field::new(COL_TARGET_KEY, DataType::Int64, false),
        Field::new(COL_VALID_TIME, ts_datatype(), true),
    ];
    fields.extend(prop_schema.fields());
    let schema = Arc::new(Schema::new_with_metadata(
        fields,
        file_metadata("current", "edge"),
    ));

    let mut writer = StreamWriter::create(path, schema, batch_size)?;
    let mut report = ExportReport::default();

    let mut edge_types: Vec<Option<String>> = Vec::new();
    let mut sources: Vec<i64> = Vec::new();
    let mut targets: Vec<i64> = Vec::new();
    let mut valid_times: Vec<Option<i64>> = Vec::new();
    let mut props = PropColumns::new_from_prop_schema(&prop_schema);

    for &nid in &node_ids {
        for eid in db.get_outgoing_edges(nid) {
            let Ok(edge) = db.get_edge(eid) else { continue };
            edge_types.push(GLOBAL_INTERNER.resolve_with(edge.label, |s| s.to_string()));
            sources.push(edge.source.as_u64() as i64);
            targets.push(edge.target.as_u64() as i64);
            valid_times.push(current_valid_from_edge(db, edge.current_version));
            props.push(&edge.properties);
            report.edges_exported += 1;

            if edge_types.len() >= batch_size {
                flush_edges(
                    &mut writer,
                    &mut edge_types,
                    &mut sources,
                    &mut targets,
                    &mut valid_times,
                    &mut props,
                )?;
            }
        }
    }
    if !edge_types.is_empty() {
        flush_edges(
            &mut writer,
            &mut edge_types,
            &mut sources,
            &mut targets,
            &mut valid_times,
            &mut props,
        )?;
    }

    writer.close()?;
    Ok(report)
}

#[allow(clippy::too_many_arguments)]
fn flush_edges(
    writer: &mut StreamWriter,
    edge_types: &mut Vec<Option<String>>,
    sources: &mut Vec<i64>,
    targets: &mut Vec<i64>,
    valid_times: &mut Vec<Option<i64>>,
    props: &mut PropColumns,
) -> Result<()> {
    let mut arrays: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from_iter(std::mem::take(edge_types))),
        Arc::new(Int64Array::from(std::mem::take(sources))),
        Arc::new(Int64Array::from(std::mem::take(targets))),
        ts_array(std::mem::take(valid_times)),
    ];
    props.finish_into(&mut arrays);
    writer.write(arrays)
}

// ============================================================================
// History export (nodes and edges)
// ============================================================================

const NODE_HISTORY_RESERVED: &[&str] = &[
    COL_ID,
    COL_LABEL,
    COL_VERSION,
    COL_VALID_FROM,
    COL_VALID_TO,
    COL_TRANSACTION_TIME,
    COL_TOMBSTONE,
    COL_PROV_SOURCE,
    COL_PROV_CONFIDENCE,
    COL_PROV_NOTE,
    COL_PROV_CORRELATION_ID,
    COL_PROV_PRINCIPAL,
    COL_PROPERTIES_JSON,
];

const EDGE_HISTORY_RESERVED: &[&str] = &[
    COL_ID,
    COL_EDGE_TYPE,
    COL_SOURCE_KEY,
    COL_TARGET_KEY,
    COL_VERSION,
    COL_VALID_FROM,
    COL_VALID_TO,
    COL_TRANSACTION_TIME,
    COL_TOMBSTONE,
    COL_PROV_SOURCE,
    COL_PROV_CONFIDENCE,
    COL_PROV_NOTE,
    COL_PROV_CORRELATION_ID,
    COL_PROV_PRINCIPAL,
    COL_PROPERTIES_JSON,
];

/// The bi-temporal + provenance tail columns shared by node and edge history.
fn history_tail_fields() -> Vec<Field> {
    vec![
        Field::new(COL_VERSION, DataType::Int64, false),
        Field::new(COL_VALID_FROM, ts_datatype(), true),
        Field::new(COL_VALID_TO, ts_datatype(), true),
        Field::new(COL_TRANSACTION_TIME, ts_datatype(), true),
        Field::new(COL_TOMBSTONE, DataType::Boolean, false),
        Field::new(COL_PROV_SOURCE, DataType::Utf8, true),
        Field::new(COL_PROV_CONFIDENCE, DataType::Float64, true),
        Field::new(COL_PROV_NOTE, DataType::Utf8, true),
        Field::new(COL_PROV_CORRELATION_ID, DataType::Utf8, true),
        Field::new(COL_PROV_PRINCIPAL, DataType::Utf8, true),
    ]
}

/// Column accumulators for the shared history tail.
#[derive(Default)]
struct HistoryTail {
    versions: Vec<i64>,
    valid_from: Vec<Option<i64>>,
    valid_to: Vec<Option<i64>>,
    transaction_time: Vec<Option<i64>>,
    tombstone: Vec<bool>,
    prov_source: Vec<Option<String>>,
    prov_confidence: Vec<Option<f64>>,
    prov_note: Vec<Option<String>>,
    prov_correlation_id: Vec<Option<String>>,
    prov_principal: Vec<Option<String>>,
}

impl HistoryTail {
    fn push(&mut self, version: &VersionInfo) {
        let valid = version.temporal.valid_time();
        let tx = version.temporal.transaction_time();
        self.versions.push(version.version_number as i64);
        self.valid_from.push(Some(valid.start().wallclock()));
        // Open interval => null (never the TIMESTAMP_MAX sentinel).
        self.valid_to.push(if valid.is_current() {
            None
        } else {
            Some(valid.end().wallclock())
        });
        self.transaction_time.push(Some(tx.start().wallclock()));
        // A closed valid interval marks the fact's real-world validity as ended
        // (deletion / retraction). See the module docs for the exact convention.
        self.tombstone.push(!valid.is_current());

        let prov = version.provenance.as_ref();
        self.prov_source
            .push(prov.and_then(Provenance::source).map(str::to_string));
        self.prov_confidence
            .push(prov.and_then(Provenance::confidence));
        self.prov_note
            .push(prov.and_then(Provenance::note).map(str::to_string));
        self.prov_correlation_id.push(
            prov.and_then(Provenance::correlation_id)
                .map(str::to_string),
        );
        self.prov_principal
            .push(prov.and_then(Provenance::principal).map(str::to_string));
    }

    fn len(&self) -> usize {
        self.versions.len()
    }

    fn finish_into(&mut self, arrays: &mut Vec<ArrayRef>) {
        arrays.push(Arc::new(Int64Array::from(std::mem::take(
            &mut self.versions,
        ))));
        arrays.push(ts_array(std::mem::take(&mut self.valid_from)));
        arrays.push(ts_array(std::mem::take(&mut self.valid_to)));
        arrays.push(ts_array(std::mem::take(&mut self.transaction_time)));
        arrays.push(Arc::new(BooleanArray::from(std::mem::take(
            &mut self.tombstone,
        ))));
        arrays.push(Arc::new(StringArray::from_iter(std::mem::take(
            &mut self.prov_source,
        ))));
        arrays.push(Arc::new(Float64Array::from(std::mem::take(
            &mut self.prov_confidence,
        ))));
        arrays.push(Arc::new(StringArray::from_iter(std::mem::take(
            &mut self.prov_note,
        ))));
        arrays.push(Arc::new(StringArray::from_iter(std::mem::take(
            &mut self.prov_correlation_id,
        ))));
        arrays.push(Arc::new(StringArray::from_iter(std::mem::take(
            &mut self.prov_principal,
        ))));
    }
}

pub(super) fn export_node_history(
    db: &AletheiaDB,
    path: &Path,
    batch_size: usize,
) -> Result<ExportReport> {
    // Enumerate every node ever recorded (incl. deleted/retracted) via the changefeed.
    let node_ids: Vec<NodeId> = collect_history_ids(db, EntityKind::Node)?
        .into_iter()
        .filter_map(|id| NodeId::new(id).ok())
        .collect();

    // Pass 1: property schema over all versions of all nodes.
    let mut builder = SchemaBuilder::new(NODE_HISTORY_RESERVED);
    for &id in &node_ids {
        if let Ok(history) = db.get_node_history(id) {
            for version in &history.versions {
                builder.observe(&version.properties);
            }
        }
    }
    let prop_schema = builder.finish();

    // Schema: id, label, [props], <tail>.
    let mut fields = vec![
        Field::new(COL_ID, DataType::Int64, false),
        Field::new(COL_LABEL, DataType::Utf8, false),
    ];
    let prop_fields = prop_schema.fields();
    fields.extend(prop_fields.clone());
    fields.extend(history_tail_fields());
    let schema = Arc::new(Schema::new_with_metadata(
        fields,
        file_metadata("history", "node"),
    ));

    let mut writer = StreamWriter::create(path, schema, batch_size)?;
    let mut report = ExportReport::default();

    let mut ids: Vec<i64> = Vec::new();
    let mut labels: Vec<Option<String>> = Vec::new();
    let mut props = PropColumns::new_from_prop_schema(&prop_schema);
    let mut tail = HistoryTail::default();

    for &id in &node_ids {
        let Ok(history) = db.get_node_history(id) else {
            continue;
        };
        for version in &history.versions {
            ids.push(id.as_u64() as i64);
            labels.push(Some(version.label.clone()));
            props.push(&version.properties);
            tail.push(version);
            report.node_versions_exported += 1;

            if ids.len() >= batch_size {
                let mut arrays: Vec<ArrayRef> = vec![
                    Arc::new(Int64Array::from(std::mem::take(&mut ids))),
                    Arc::new(StringArray::from_iter(std::mem::take(&mut labels))),
                ];
                props.finish_into(&mut arrays);
                tail.finish_into(&mut arrays);
                writer.write(arrays)?;
            }
        }
    }
    if !ids.is_empty() || tail.len() > 0 {
        let mut arrays: Vec<ArrayRef> = vec![
            Arc::new(Int64Array::from(std::mem::take(&mut ids))),
            Arc::new(StringArray::from_iter(std::mem::take(&mut labels))),
        ];
        props.finish_into(&mut arrays);
        tail.finish_into(&mut arrays);
        writer.write(arrays)?;
    }

    writer.close()?;
    Ok(report)
}

pub(super) fn export_edge_history(
    db: &AletheiaDB,
    path: &Path,
    batch_size: usize,
) -> Result<ExportReport> {
    // Enumerate every edge ever recorded (incl. deleted) via the changefeed, so a
    // deleted edge's history is still exported. Endpoints for an edge no longer in
    // current state are written as null (they are not carried on a historical version).
    let edge_ids: Vec<EdgeId> = collect_history_ids(db, EntityKind::Edge)?
        .into_iter()
        .filter_map(|id| EdgeId::new(id).ok())
        .collect();

    // Pass 1: property schema over all versions of all edges.
    let mut builder = SchemaBuilder::new(EDGE_HISTORY_RESERVED);
    for &eid in &edge_ids {
        if let Ok(history) = db.get_edge_history(eid) {
            for version in &history.versions {
                builder.observe(&version.properties);
            }
        }
    }
    let prop_schema = builder.finish();

    // Schema: id, edge_type, source_key, target_key, [props], <tail>.
    let mut fields = vec![
        Field::new(COL_ID, DataType::Int64, false),
        Field::new(COL_EDGE_TYPE, DataType::Utf8, false),
        Field::new(COL_SOURCE_KEY, DataType::Int64, true),
        Field::new(COL_TARGET_KEY, DataType::Int64, true),
    ];
    fields.extend(prop_schema.fields());
    fields.extend(history_tail_fields());
    let schema = Arc::new(Schema::new_with_metadata(
        fields,
        file_metadata("history", "edge"),
    ));

    let mut writer = StreamWriter::create(path, schema, batch_size)?;
    let mut report = ExportReport::default();

    let mut ids: Vec<i64> = Vec::new();
    let mut edge_types: Vec<Option<String>> = Vec::new();
    let mut sources: Vec<Option<i64>> = Vec::new();
    let mut targets: Vec<Option<i64>> = Vec::new();
    let mut props = PropColumns::new_from_prop_schema(&prop_schema);
    let mut tail = HistoryTail::default();

    for &eid in &edge_ids {
        let Ok(history) = db.get_edge_history(eid) else {
            continue;
        };
        // Endpoints come from the current edge when it is still live; a deleted edge's
        // endpoints are not carried on a historical version, so they are null.
        let (source, target) = match db.get_edge(eid) {
            Ok(edge) => (
                Some(edge.source.as_u64() as i64),
                Some(edge.target.as_u64() as i64),
            ),
            Err(_) => (None, None),
        };
        for version in &history.versions {
            ids.push(eid.as_u64() as i64);
            edge_types.push(Some(version.label.clone()));
            sources.push(source);
            targets.push(target);
            props.push(&version.properties);
            tail.push(version);
            report.edge_versions_exported += 1;

            if ids.len() >= batch_size {
                flush_edge_history(
                    &mut writer,
                    &mut ids,
                    &mut edge_types,
                    &mut sources,
                    &mut targets,
                    &mut props,
                    &mut tail,
                )?;
            }
        }
    }
    if !ids.is_empty() || tail.len() > 0 {
        flush_edge_history(
            &mut writer,
            &mut ids,
            &mut edge_types,
            &mut sources,
            &mut targets,
            &mut props,
            &mut tail,
        )?;
    }

    writer.close()?;
    Ok(report)
}

#[allow(clippy::too_many_arguments)]
fn flush_edge_history(
    writer: &mut StreamWriter,
    ids: &mut Vec<i64>,
    edge_types: &mut Vec<Option<String>>,
    sources: &mut Vec<Option<i64>>,
    targets: &mut Vec<Option<i64>>,
    props: &mut PropColumns,
    tail: &mut HistoryTail,
) -> Result<()> {
    let mut arrays: Vec<ArrayRef> = vec![
        Arc::new(Int64Array::from(std::mem::take(ids))),
        Arc::new(StringArray::from_iter(std::mem::take(edge_types))),
        Arc::new(Int64Array::from(std::mem::take(sources))),
        Arc::new(Int64Array::from(std::mem::take(targets))),
    ];
    props.finish_into(&mut arrays);
    tail.finish_into(&mut arrays);
    writer.write(arrays)
}

// ============================================================================
// Shared helpers
// ============================================================================

impl PropColumns {
    /// Convenience constructor used by the export pipelines.
    fn new_from_prop_schema(schema: &PropSchema) -> Self {
        PropColumns::new(schema)
    }
}

/// Page size for the changefeed enumeration driving history export.
const CHANGEFEED_PAGE: usize = 4096;

/// Enumerate the **distinct** entity ids of the requested `kind` across all recorded
/// history — including entities that were deleted or retracted and are therefore no
/// longer in current state — by paging the whole-window changefeed.
///
/// Returned ids are ascending (deterministic). Any residual completeness caveat is the
/// changefeed's own (a version's transaction time must fall inside `[0, TIMESTAMP_MAX)`,
/// which every committed version does; cold-tier versions are merged by `list_changes`).
fn collect_history_ids(db: &AletheiaDB, kind: EntityKind) -> Result<Vec<u64>> {
    let mut ids = std::collections::BTreeSet::new();
    let mut cursor: Option<String> = None;
    loop {
        let mut query = ChangeFeedQuery::new(Timestamp::from(0), TIMESTAMP_MAX, CHANGEFEED_PAGE);
        query.cursor = cursor.take();
        let page = db.list_changes(&query)?;
        for record in &page.changes {
            if record.kind == kind {
                ids.insert(record.entity_id);
            }
        }
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    Ok(ids.into_iter().collect())
}

/// The current node version's valid-time start (epoch micros), if resolvable.
fn current_valid_from_node(db: &AletheiaDB, version_id: crate::core::id::VersionId) -> Option<i64> {
    match db.get_node_version_read_metadata(version_id) {
        Ok(Some((_, interval))) => Some(interval.valid_time().start().wallclock()),
        _ => None,
    }
}

/// The current edge version's valid-time start (epoch micros), if resolvable.
fn current_valid_from_edge(db: &AletheiaDB, version_id: crate::core::id::VersionId) -> Option<i64> {
    match db.get_edge_version_read_metadata(version_id) {
        Ok(Some((_, interval))) => Some(interval.valid_time().start().wallclock()),
        _ => None,
    }
}
