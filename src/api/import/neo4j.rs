//! Neo4j CSV migration importer (Issue #3356).
//!
//! Loads a Neo4j **CSV** export — the `neo4j-admin database import` /
//! `apoc.export.csv` self-describing typed-header convention — into AletheiaDB,
//! reusing the #3211 bulk-import framework wholesale. The Neo4j header is
//! self-describing, so (unlike the generic CSV importer) the caller does **not**
//! hand-write property/type mappings: this module parses the header, derives a
//! per-column coercion plan, and streams rows through the existing chunked ACID
//! commit loops (`import_nodes_with` / `import_edges_with`).
//!
//! # Scope
//!
//! CSV only. A binary Neo4j `.dump` archive and the APOC Cypher-script dump
//! (`apoc.export.cypher.all`) are **rejected** with a clear, actionable error
//! and tracked as documented follow-ups (see the CLI and
//! `docs/guides/neo4j-import.md`).
//!
//! # Header + type handling
//!
//! Reserved headers: `:ID`, `:ID(space)`, `:LABEL`, `:START_ID`/`:START_ID(space)`,
//! `:END_ID`/`:END_ID(space)`, `:TYPE`, `:IGNORE`. Typed property columns follow
//! `name:type` (untyped defaults to `string`) and `name:type[]` for arrays.
//! `duration`, `point`/spatial, and `byte[]` are **rejected** and reported as
//! `unsupported` with counts; `--strict-types` upgrades that to a hard error.
//! Multi-label `:LABEL` cells (`Person;Employee`) flatten deterministically
//! (first label wins by default) while the full set is preserved as a `_labels`
//! array property. Numeric arrays become vectors **only** via an explicit
//! `vector_property` opt-in, never silently.

use std::collections::HashSet;
use std::path::Path;

use super::parse::{self, Row};
use super::{
    EdgePrepError, Endpoint, Importer, PreparedEdge, PreparedNode, RowError, UnresolvedEndpoint,
};
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;
use crate::core::property::{PropertyMapBuilder, PropertyValue};
use crate::core::provenance::Provenance;

/// How a multi-label Neo4j node's label set collapses onto AletheiaDB's
/// single-label model (Issue #3356).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LabelStrategy {
    /// The **first** label in the `:LABEL` cell becomes the node label; the full
    /// set is preserved as a `_labels` array property (default, deterministic,
    /// lossless).
    #[default]
    First,
    /// The labels are joined with `_` into a single synthetic label
    /// (`Person;Employee` -> `Person_Employee`).
    Concat,
    /// The first label becomes the node label and the full set is **always**
    /// stored as the `_labels` array property.
    Property,
}

/// Options controlling a Neo4j CSV import (Issue #3356).
///
/// Constructed via [`Neo4jCsvOptions::new`] / [`Default`] and the fluent
/// `with_*` methods. The defaults mirror `neo4j-admin database import`
/// (comma field delimiter, `;` array delimiter, `"` quote, first-label wins).
#[derive(Debug, Clone)]
pub struct Neo4jCsvOptions {
    /// CSV field delimiter (default `b','`).
    pub delimiter: u8,
    /// Delimiter separating array elements and multiple labels (default `';'`).
    pub array_delimiter: char,
    /// CSV quote character (default `b'"'`, RFC-4180 doubled-quote escaping).
    pub quote: u8,
    /// How multi-label nodes collapse onto the single-label model.
    pub label_strategy: LabelStrategy,
    /// Property key under which the full label set is preserved (default
    /// `"_labels"`).
    pub retained_labels_key: String,
    /// Property names whose numeric array columns become dense [`PropertyValue::Vector`]s
    /// instead of [`PropertyValue::Array`]. Explicit opt-in only (AC2).
    pub vector_properties: HashSet<String>,
    /// Optional property name of a `date`/`datetime` column designating each
    /// row's `valid_from` (Issue #3221). The column is consumed as valid time
    /// and not also stored as a property.
    pub valid_from_property: Option<String>,
    /// When `true`, an unsupported type in the header is a hard error at
    /// header-parse time (before any transaction opens) instead of a
    /// reported-and-counted skip.
    pub strict_types: bool,
}

impl Default for Neo4jCsvOptions {
    fn default() -> Self {
        Neo4jCsvOptions {
            delimiter: b',',
            array_delimiter: ';',
            quote: b'"',
            label_strategy: LabelStrategy::First,
            retained_labels_key: "_labels".to_string(),
            vector_properties: HashSet::new(),
            valid_from_property: None,
            strict_types: false,
        }
    }
}

impl Neo4jCsvOptions {
    /// Create default Neo4j CSV import options.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the CSV field delimiter.
    #[must_use]
    pub fn delimiter(mut self, delimiter: u8) -> Self {
        self.delimiter = delimiter;
        self
    }

    /// Set the CSV quote character.
    #[must_use]
    pub fn quote(mut self, quote: u8) -> Self {
        self.quote = quote;
        self
    }

    /// Set the array / multi-label element delimiter.
    #[must_use]
    pub fn array_delimiter(mut self, array_delimiter: char) -> Self {
        self.array_delimiter = array_delimiter;
        self
    }

    /// Set the multi-label collapse strategy.
    #[must_use]
    pub fn label_strategy(mut self, strategy: LabelStrategy) -> Self {
        self.label_strategy = strategy;
        self
    }

    /// Set the property key used to preserve the full label set.
    #[must_use]
    pub fn retained_labels_key(mut self, key: impl Into<String>) -> Self {
        self.retained_labels_key = key.into();
        self
    }

    /// Mark a numeric-array property to be stored as a dense vector (AC2).
    #[must_use]
    pub fn vector_property(mut self, name: impl Into<String>) -> Self {
        self.vector_properties.insert(name.into());
        self
    }

    /// Designate a `date`/`datetime` property as each row's `valid_from`.
    #[must_use]
    pub fn valid_from_property(mut self, name: impl Into<String>) -> Self {
        self.valid_from_property = Some(name.into());
        self
    }

    /// Turn unsupported-type headers into a hard error.
    #[must_use]
    pub fn strict_types(mut self, strict: bool) -> Self {
        self.strict_types = strict;
        self
    }
}

/// One entry of the neo4j-label(set) -> AletheiaDB-label mapping table.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LabelMapEntry {
    /// The Neo4j label set as written in `:LABEL` (delimiter-joined).
    pub neo4j_labels: String,
    /// The AletheiaDB label chosen for those nodes.
    pub aletheia_label: String,
    /// Number of nodes mapped this way.
    pub count: usize,
}

/// One entry of the neo4j-rel-type -> AletheiaDB-label mapping table.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TypeMapEntry {
    /// The Neo4j relationship type.
    pub neo4j_type: String,
    /// The AletheiaDB edge label (identical to the type today).
    pub aletheia_label: String,
    /// Number of edges mapped this way.
    pub count: usize,
}

/// A value that was accepted but transformed on the way in (Issue #3356 AC4).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CoercionNote {
    /// The source column header (or a synthetic `:LABEL` marker).
    pub column: String,
    /// A stable machine token identifying the coercion kind
    /// (`char_to_string`, `temporal_to_micros`, `time_to_string`,
    /// `multi_label_flattened`).
    pub kind: String,
    /// A human-readable description of the transformation.
    pub detail: String,
    /// Number of rows affected.
    pub count: usize,
}

/// A column whose Neo4j type has no AletheiaDB representation and was dropped
/// (Issue #3356 AC8) — never silently, always counted.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct UnsupportedNote {
    /// The source column header.
    pub column: String,
    /// The unsupported Neo4j type (`duration`, `point`, `byte[]`, ...).
    pub neo4j_type: String,
    /// Number of rows that carried a (non-empty) value in this column.
    pub count: usize,
}

/// Machine-readable fidelity report for a Neo4j CSV import (Issue #3356 AC4).
///
/// A superset of the generic [`ImportReport`](super::ImportReport): it adds the
/// label/type mapping tables, coercion and unsupported-type notes, and a
/// `zero_loss` boolean that is `true` iff nothing was skipped, left unresolved,
/// or dropped as unsupported.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Neo4jFidelityReport {
    /// Total input data rows read across all node + relationship files.
    pub rows_read: usize,
    /// Number of nodes successfully imported.
    pub nodes_imported: usize,
    /// Number of relationships successfully imported.
    pub relationships_imported: usize,
    /// Number of property values successfully written.
    pub properties_imported: usize,
    /// neo4j-label(set) -> AletheiaDB-label mapping table with counts.
    pub label_mapping: Vec<LabelMapEntry>,
    /// neo4j-rel-type -> AletheiaDB-label mapping table with counts.
    pub type_mapping: Vec<TypeMapEntry>,
    /// Malformed rows skipped (populated in skip-and-report mode).
    pub skipped: Vec<RowError>,
    /// Edges whose endpoints did not resolve (populated in skip-and-report mode).
    pub unresolved_endpoints: Vec<UnresolvedEndpoint>,
    /// Values accepted but transformed (char, temporal, multi-label...).
    pub coerced: Vec<CoercionNote>,
    /// Columns dropped because their Neo4j type is unsupported.
    pub unsupported: Vec<UnsupportedNote>,
    /// `true` iff `skipped`, `unresolved_endpoints`, and `unsupported` are all
    /// empty — a lossless import.
    pub zero_loss: bool,
}

impl Neo4jFidelityReport {
    /// Recompute [`zero_loss`](Self::zero_loss) from the accumulated notes.
    fn finalize(&mut self) {
        self.zero_loss = self.skipped.is_empty()
            && self.unresolved_endpoints.is_empty()
            && self.unsupported.is_empty();
    }

    fn note_label(&mut self, neo4j_labels: &str, aletheia_label: &str) {
        if let Some(entry) = self
            .label_mapping
            .iter_mut()
            .find(|e| e.neo4j_labels == neo4j_labels && e.aletheia_label == aletheia_label)
        {
            entry.count += 1;
        } else {
            self.label_mapping.push(LabelMapEntry {
                neo4j_labels: neo4j_labels.to_string(),
                aletheia_label: aletheia_label.to_string(),
                count: 1,
            });
        }
    }

    fn note_type(&mut self, neo4j_type: &str) {
        if let Some(entry) = self
            .type_mapping
            .iter_mut()
            .find(|e| e.neo4j_type == neo4j_type)
        {
            entry.count += 1;
        } else {
            self.type_mapping.push(TypeMapEntry {
                neo4j_type: neo4j_type.to_string(),
                aletheia_label: neo4j_type.to_string(),
                count: 1,
            });
        }
    }

    fn note_coercion(&mut self, column: &str, kind: &str, detail: &str) {
        if let Some(entry) = self
            .coerced
            .iter_mut()
            .find(|e| e.column == column && e.kind == kind)
        {
            entry.count += 1;
        } else {
            self.coerced.push(CoercionNote {
                column: column.to_string(),
                kind: kind.to_string(),
                detail: detail.to_string(),
                count: 1,
            });
        }
    }

    fn note_unsupported(&mut self, column: &str, neo4j_type: &str) {
        if let Some(entry) = self.unsupported.iter_mut().find(|e| e.column == column) {
            entry.count += 1;
        } else {
            self.unsupported.push(UnsupportedNote {
                column: column.to_string(),
                neo4j_type: neo4j_type.to_string(),
                count: 1,
            });
        }
    }
}

// ---- Header model ----------------------------------------------------------

/// A supported scalar Neo4j type, after header classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NeoScalar {
    /// `string` and untyped columns.
    Str,
    /// `int`, `long`, `short`, `byte` (scalar).
    Int,
    /// `float`, `double`.
    Float,
    /// `boolean`.
    Bool,
    /// `char` -> 1-char string.
    Char,
    /// `date`, `datetime`, `localdatetime` -> epoch micros.
    DateTime,
    /// `time`, `localtime` -> string (no epoch anchor).
    TimeString,
}

/// A parsed header column.
#[derive(Debug, Clone)]
enum ColKind {
    Id {
        group: Option<String>,
        property_name: Option<String>,
    },
    Label,
    StartId {
        group: Option<String>,
    },
    EndId {
        group: Option<String>,
    },
    Type,
    Ignore,
    Property {
        name: String,
        ty: NeoScalar,
        is_array: bool,
    },
    Unsupported {
        type_str: String,
    },
}

#[derive(Debug, Clone)]
struct Column {
    header: String,
    kind: ColKind,
}

/// Split a header type token into `(base, group, is_array)`.
///
/// `ID(Person)` -> `("ID", Some("Person"), false)`; `int[]` -> `("int", None, true)`.
fn split_type_token(token: &str) -> (String, Option<String>, bool) {
    let mut base = token.to_string();
    let mut group = None;
    let is_array = base.contains("[]");
    base = base.replace("[]", "");
    if let (Some(open), Some(close)) = (base.find('('), base.rfind(')'))
        && close > open
    {
        group = Some(base[open + 1..close].to_string());
        base = base[..open].to_string();
    }
    (base.trim().to_string(), group, is_array)
}

/// Classify a scalar Neo4j type name into a [`NeoScalar`], or `None` if
/// unsupported (`duration`, `point`, or `byte[]`).
fn classify_scalar(base_lower: &str, is_array: bool) -> Option<NeoScalar> {
    match base_lower {
        "string" | "" => Some(NeoScalar::Str),
        "int" | "long" | "short" => Some(NeoScalar::Int),
        "byte" => {
            if is_array {
                None // byte[] is unsupported (AC8)
            } else {
                Some(NeoScalar::Int)
            }
        }
        "float" | "double" => Some(NeoScalar::Float),
        "boolean" => Some(NeoScalar::Bool),
        "char" => Some(NeoScalar::Char),
        "date" | "datetime" | "localdatetime" => Some(NeoScalar::DateTime),
        "time" | "localtime" => Some(NeoScalar::TimeString),
        _ => None, // duration, point, and any unknown type
    }
}

/// Parse one CSV header field into a [`ColKind`].
fn parse_column(header: &str) -> std::result::Result<ColKind, String> {
    let trimmed = header.trim();
    let (name_part, type_part) = match trimmed.split_once(':') {
        Some((name, ty)) => (name.trim(), ty.trim()),
        None => {
            if trimmed.is_empty() {
                return Err("empty header field".to_string());
            }
            // Untyped column defaults to a string property.
            return Ok(ColKind::Property {
                name: trimmed.to_string(),
                ty: NeoScalar::Str,
                is_array: false,
            });
        }
    };

    let (base, group, is_array) = split_type_token(type_part);
    match base.to_ascii_uppercase().as_str() {
        "ID" => Ok(ColKind::Id {
            group,
            property_name: if name_part.is_empty() {
                None
            } else {
                Some(name_part.to_string())
            },
        }),
        "LABEL" => Ok(ColKind::Label),
        "START_ID" => Ok(ColKind::StartId { group }),
        "END_ID" => Ok(ColKind::EndId { group }),
        "TYPE" => Ok(ColKind::Type),
        "IGNORE" => Ok(ColKind::Ignore),
        _ => {
            // A property column: name is required.
            if name_part.is_empty() {
                return Err(format!("property column '{header}' is missing a name"));
            }
            let base_lower = base.to_ascii_lowercase();
            match classify_scalar(&base_lower, is_array) {
                Some(ty) => Ok(ColKind::Property {
                    name: name_part.to_string(),
                    ty,
                    is_array,
                }),
                None => {
                    let type_str = if is_array {
                        format!("{base_lower}[]")
                    } else {
                        base_lower
                    };
                    Ok(ColKind::Unsupported { type_str })
                }
            }
        }
    }
}

fn parse_columns(headers: &[String]) -> std::result::Result<Vec<Column>, String> {
    if headers.is_empty() || (headers.len() == 1 && headers[0].trim().is_empty()) {
        return Err("missing header row".to_string());
    }
    headers
        .iter()
        .map(|h| {
            parse_column(h).map(|kind| Column {
                header: h.clone(),
                kind,
            })
        })
        .collect()
}

/// If strict types is on, reject the first unsupported column as a hard error.
fn enforce_strict_types(columns: &[Column]) -> std::result::Result<(), String> {
    for col in columns {
        if let ColKind::Unsupported { type_str, .. } = &col.kind {
            return Err(format!(
                "unsupported Neo4j type '{type_str}' in column '{}'",
                col.header
            ));
        }
    }
    Ok(())
}

/// Namespace an id string within its id-space group (`""` = the global group),
/// so the same raw id in two groups never collides.
fn namespaced_key(group: Option<&str>, id: &str) -> String {
    format!("{}\0{}", group.unwrap_or(""), id)
}

// ---- Value coercion --------------------------------------------------------

fn parse_bool(s: &str) -> Option<bool> {
    match s.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "t" => Some(true),
        "false" | "0" | "no" | "f" => Some(false),
        _ => None,
    }
}

/// Coerce one scalar element string per its [`NeoScalar`] type. Returns `Ok(None)`
/// for an empty element (which is dropped). Records coercions via `on_coerce`.
fn coerce_scalar<F: FnMut(&str)>(
    raw: &str,
    ty: NeoScalar,
    on_coerce: &mut F,
) -> std::result::Result<Option<PropertyValue>, String> {
    let trimmed = raw.trim();
    match ty {
        NeoScalar::Str => {
            if raw.is_empty() {
                Ok(None)
            } else {
                Ok(Some(PropertyValue::string(raw)))
            }
        }
        NeoScalar::Int => {
            if trimmed.is_empty() {
                return Ok(None);
            }
            trimmed
                .parse::<i64>()
                .map(|v| Some(PropertyValue::Int(v)))
                .map_err(|_| format!("cannot coerce '{raw}' to Int"))
        }
        NeoScalar::Float => {
            if trimmed.is_empty() {
                return Ok(None);
            }
            trimmed
                .parse::<f64>()
                .map(|v| Some(PropertyValue::Float(v)))
                .map_err(|_| format!("cannot coerce '{raw}' to Float"))
        }
        NeoScalar::Bool => {
            if trimmed.is_empty() {
                return Ok(None);
            }
            parse_bool(trimmed)
                .map(|v| Some(PropertyValue::Bool(v)))
                .ok_or_else(|| format!("cannot coerce '{raw}' to Bool"))
        }
        NeoScalar::Char => {
            if raw.is_empty() {
                return Ok(None);
            }
            on_coerce("char_to_string");
            let ch = raw.chars().next().expect("non-empty");
            Ok(Some(PropertyValue::string(ch.to_string())))
        }
        NeoScalar::DateTime => {
            if trimmed.is_empty() {
                return Ok(None);
            }
            on_coerce("temporal_to_micros");
            let ts = parse::parse_valid_time(trimmed)?;
            Ok(Some(PropertyValue::Int(ts.wallclock())))
        }
        NeoScalar::TimeString => {
            if trimmed.is_empty() {
                return Ok(None);
            }
            on_coerce("time_to_string");
            Ok(Some(PropertyValue::string(raw)))
        }
    }
}

/// Coerce a property column's raw cell into a [`PropertyValue`], honoring
/// arrays / vectors. Returns `Ok(None)` for an absent value.
fn coerce_property<F: FnMut(&str)>(
    raw: &str,
    ty: NeoScalar,
    is_array: bool,
    array_delim: char,
    as_vector: bool,
    on_coerce: &mut F,
) -> std::result::Result<Option<PropertyValue>, String> {
    if !is_array {
        return coerce_scalar(raw, ty, on_coerce);
    }
    if raw.trim().is_empty() {
        return Ok(None);
    }
    let parts: Vec<&str> = raw.split(array_delim).collect();
    if as_vector && matches!(ty, NeoScalar::Int | NeoScalar::Float) {
        let mut floats = Vec::with_capacity(parts.len());
        for part in parts {
            let t = part.trim();
            let v = t
                .parse::<f32>()
                .map_err(|_| format!("cannot coerce '{part}' to vector element (f32)"))?;
            floats.push(v);
        }
        return Ok(Some(PropertyValue::vector(&floats)));
    }
    let mut values = Vec::with_capacity(parts.len());
    for part in parts {
        if let Some(v) = coerce_scalar(part, ty, on_coerce)? {
            values.push(v);
        }
    }
    Ok(Some(PropertyValue::array(values)))
}

// ---- Node / edge preparation -----------------------------------------------

fn prepare_node(
    row: &Row,
    columns: &[Column],
    opts: &Neo4jCsvOptions,
    report: &mut Neo4jFidelityReport,
) -> std::result::Result<PreparedNode, RowError> {
    let row_err = |message: String| RowError {
        row: row.index,
        message,
    };

    // --- label ---
    let label_col = columns
        .iter()
        .find(|c| matches!(c.kind, ColKind::Label))
        .expect("node plan validated to have :LABEL");
    let raw_label = row.get_str(&label_col.header).map_err(&row_err)?;
    let labels: Vec<String> = raw_label
        .split(opts.array_delimiter)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    if labels.is_empty() {
        return Err(row_err("node has no label".to_string()));
    }
    let chosen_label = match opts.label_strategy {
        LabelStrategy::Concat => labels.join("_"),
        LabelStrategy::First | LabelStrategy::Property => labels[0].clone(),
    };
    report.note_label(
        &labels.join(&opts.array_delimiter.to_string()),
        &chosen_label,
    );
    if labels.len() > 1 {
        report.note_coercion(
            ":LABEL",
            "multi_label_flattened",
            "multi-label node flattened; full set kept in _labels",
        );
    }

    let mut builder = PropertyMapBuilder::new();
    let mut prop_count = 0usize;

    // Preserve the full label set as `_labels` (First: only when multi-label;
    // Property: always; Concat: never, the concatenated label already holds it).
    let store_labels = match opts.label_strategy {
        LabelStrategy::First => labels.len() > 1,
        LabelStrategy::Property => true,
        LabelStrategy::Concat => false,
    };
    if store_labels {
        let arr = PropertyValue::array(labels.iter().map(PropertyValue::string).collect());
        builder = builder
            .try_insert(&opts.retained_labels_key, arr)
            .map_err(|e| row_err(e.to_string()))?;
        prop_count += 1;
    }

    // --- id / key ---
    let id_col = columns
        .iter()
        .find(|c| matches!(c.kind, ColKind::Id { .. }))
        .expect("node plan validated to have :ID");
    let (group, property_name) = match &id_col.kind {
        ColKind::Id {
            group,
            property_name,
        } => (group.clone(), property_name.clone()),
        _ => unreachable!(),
    };
    let raw_id = row.get_str(&id_col.header).map_err(&row_err)?;
    if raw_id.trim().is_empty() {
        return Err(row_err("id column ':ID' is empty".to_string()));
    }
    let key = namespaced_key(group.as_deref(), raw_id.trim());
    if let Some(pname) = &property_name {
        builder = builder
            .try_insert(pname, PropertyValue::string(raw_id.trim()))
            .map_err(|e| row_err(e.to_string()))?;
        prop_count += 1;
    }

    // --- valid_from + properties ---
    let mut valid_from = None;
    for col in columns {
        match &col.kind {
            ColKind::Property { name, ty, is_array } => {
                // Consume the valid-from column as valid time, not a property.
                if opts.valid_from_property.as_deref() == Some(name.as_str()) {
                    let raw = row.get_str(&col.header).map_err(&row_err)?;
                    if !raw.trim().is_empty() {
                        let ts = parse::parse_valid_time(raw.trim())
                            .map_err(|m| row_err(format!("column '{}': {m}", col.header)))?;
                        valid_from = Some(ts);
                    }
                    continue;
                }
                let raw = row.get_str(&col.header).map_err(&row_err)?;
                let as_vector = opts.vector_properties.contains(name);
                let mut on_coerce = |kind: &str| {
                    report.note_coercion(&col.header, kind, coercion_detail(kind));
                };
                let value = coerce_property(
                    &raw,
                    *ty,
                    *is_array,
                    opts.array_delimiter,
                    as_vector,
                    &mut on_coerce,
                )
                .map_err(|m| row_err(format!("column '{}': {m}", col.header)))?;
                if let Some(value) = value {
                    builder = builder
                        .try_insert(name, value)
                        .map_err(|e| row_err(e.to_string()))?;
                    prop_count += 1;
                }
            }
            ColKind::Unsupported { type_str, .. } => {
                let raw = row.get_str(&col.header).map_err(&row_err)?;
                if !raw.trim().is_empty() {
                    report.note_unsupported(&col.header, type_str);
                }
            }
            _ => {}
        }
    }

    report.properties_imported += prop_count;
    Ok(PreparedNode {
        key,
        label: chosen_label,
        properties: builder.build(),
        valid_from,
    })
}

fn prepare_edge(
    key_to_id: &std::collections::HashMap<String, NodeId>,
    row: &Row,
    columns: &[Column],
    start_group: Option<&str>,
    end_group: Option<&str>,
    opts: &Neo4jCsvOptions,
    report: &mut Neo4jFidelityReport,
) -> std::result::Result<PreparedEdge, EdgePrepError> {
    let row_err = |message: String| {
        EdgePrepError::Row(RowError {
            row: row.index,
            message,
        })
    };

    let type_col = columns
        .iter()
        .find(|c| matches!(c.kind, ColKind::Type))
        .expect("edge plan validated to have :TYPE");
    let raw_type = row.get_str(&type_col.header).map_err(&row_err)?;
    if raw_type.trim().is_empty() {
        return Err(row_err("type column ':TYPE' is empty".to_string()));
    }
    let label = raw_type.trim().to_string();
    report.note_type(&label);

    let start_col = columns
        .iter()
        .find(|c| matches!(c.kind, ColKind::StartId { .. }))
        .expect("edge plan validated to have :START_ID");
    let end_col = columns
        .iter()
        .find(|c| matches!(c.kind, ColKind::EndId { .. }))
        .expect("edge plan validated to have :END_ID");

    let raw_src = row.get_str(&start_col.header).map_err(&row_err)?;
    let raw_dst = row.get_str(&end_col.header).map_err(&row_err)?;
    let src_key = namespaced_key(start_group, raw_src.trim());
    let dst_key = namespaced_key(end_group, raw_dst.trim());

    let source = key_to_id.get(&src_key).copied().ok_or_else(|| {
        EdgePrepError::Unresolved(UnresolvedEndpoint {
            row: row.index,
            key: raw_src.trim().to_string(),
            side: Endpoint::Source,
        })
    })?;
    let target = key_to_id.get(&dst_key).copied().ok_or_else(|| {
        EdgePrepError::Unresolved(UnresolvedEndpoint {
            row: row.index,
            key: raw_dst.trim().to_string(),
            side: Endpoint::Target,
        })
    })?;

    let mut builder = PropertyMapBuilder::new();
    let mut prop_count = 0usize;
    let mut valid_from = None;
    for col in columns {
        match &col.kind {
            ColKind::Property { name, ty, is_array } => {
                if opts.valid_from_property.as_deref() == Some(name.as_str()) {
                    let raw = row.get_str(&col.header).map_err(&row_err)?;
                    if !raw.trim().is_empty() {
                        let ts = parse::parse_valid_time(raw.trim())
                            .map_err(|m| row_err(format!("column '{}': {m}", col.header)))?;
                        valid_from = Some(ts);
                    }
                    continue;
                }
                let raw = row.get_str(&col.header).map_err(&row_err)?;
                let as_vector = opts.vector_properties.contains(name);
                let mut on_coerce = |kind: &str| {
                    report.note_coercion(&col.header, kind, coercion_detail(kind));
                };
                let value = coerce_property(
                    &raw,
                    *ty,
                    *is_array,
                    opts.array_delimiter,
                    as_vector,
                    &mut on_coerce,
                )
                .map_err(|m| row_err(format!("column '{}': {m}", col.header)))?;
                if let Some(value) = value {
                    builder = builder
                        .try_insert(name, value)
                        .map_err(|e| row_err(e.to_string()))?;
                    prop_count += 1;
                }
            }
            ColKind::Unsupported { type_str, .. } => {
                let raw = row.get_str(&col.header).map_err(&row_err)?;
                if !raw.trim().is_empty() {
                    report.note_unsupported(&col.header, type_str);
                }
            }
            _ => {}
        }
    }

    report.properties_imported += prop_count;
    Ok(PreparedEdge {
        source,
        target,
        label,
        properties: builder.build(),
        valid_from,
    })
}

fn coercion_detail(kind: &str) -> &'static str {
    match kind {
        "char_to_string" => "char coerced to a 1-char string",
        "temporal_to_micros" => "date/datetime coerced to epoch microseconds",
        "time_to_string" => "time-of-day stored as a string (no epoch anchor)",
        "multi_label_flattened" => "multi-label node flattened; full set kept in _labels",
        _ => "value coerced",
    }
}

/// Derive the `neo4j-import::<basename>` provenance source for a file.
fn provenance_for(path: &Path) -> Result<Provenance> {
    let basename = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string());
    Provenance::builder()
        .source(format!("neo4j-import::{basename}"))
        .build()
        .map_err(|e| Error::other(e.to_string()))
}

impl<'a> Importer<'a> {
    /// Import Neo4j-CSV nodes from a single file (Issue #3356).
    ///
    /// Parses the self-describing header, derives a coercion plan, and streams
    /// rows through the shared chunked-commit path, stamping
    /// `neo4j-import::<file>` provenance. Returns the fidelity report.
    pub fn neo4j_nodes_from_csv(
        &mut self,
        path: impl AsRef<Path>,
        opts: &Neo4jCsvOptions,
    ) -> Result<Neo4jFidelityReport> {
        let mut report = Neo4jFidelityReport::default();
        self.run_nodes(path.as_ref(), opts, &mut report)?;
        report.finalize();
        Ok(report)
    }

    /// Import Neo4j-CSV relationships from a single file (Issue #3356).
    ///
    /// Endpoints resolve against nodes imported earlier in this session (call
    /// [`neo4j_nodes_from_csv`](Self::neo4j_nodes_from_csv) first).
    pub fn neo4j_edges_from_csv(
        &mut self,
        path: impl AsRef<Path>,
        opts: &Neo4jCsvOptions,
    ) -> Result<Neo4jFidelityReport> {
        let mut report = Neo4jFidelityReport::default();
        self.run_edges(path.as_ref(), opts, &mut report)?;
        report.finalize();
        Ok(report)
    }

    /// Import a multi-file Neo4j CSV dataset: all node files first (building the
    /// business-key map), then all relationship files. Returns one merged
    /// fidelity report (Issue #3356).
    pub fn neo4j_import_csv(
        &mut self,
        nodes: &[std::path::PathBuf],
        rels: &[std::path::PathBuf],
        opts: &Neo4jCsvOptions,
    ) -> Result<Neo4jFidelityReport> {
        let mut report = Neo4jFidelityReport::default();
        for path in nodes {
            self.run_nodes(path, opts, &mut report)?;
        }
        for path in rels {
            self.run_edges(path, opts, &mut report)?;
        }
        report.finalize();
        Ok(report)
    }

    fn run_nodes(
        &mut self,
        path: &Path,
        opts: &Neo4jCsvOptions,
        report: &mut Neo4jFidelityReport,
    ) -> Result<()> {
        let headers = parse::read_csv_header(path, opts.delimiter, opts.quote)?;
        let columns = parse_columns(&headers).map_err(Error::other)?;
        if !columns.iter().any(|c| matches!(c.kind, ColKind::Id { .. })) {
            return Err(Error::other("node file has no :ID column".to_string()));
        }
        if !columns.iter().any(|c| matches!(c.kind, ColKind::Label)) {
            return Err(Error::other("node file has no :LABEL column".to_string()));
        }
        if opts.strict_types {
            enforce_strict_types(&columns).map_err(Error::other)?;
        }

        let rows = parse::csv_rows_with(path, opts.delimiter, opts.quote)?;
        let prev_prov = self.config.provenance.take();
        self.config.provenance = Some(provenance_for(path)?);

        let columns_ref = &columns;
        let result =
            self.import_nodes_with(rows, |row| prepare_node(row, columns_ref, opts, report));

        self.config.provenance = prev_prov;
        let import_report = result?;

        report.rows_read += import_report.rows_read;
        report.nodes_imported += import_report.nodes_imported;
        report.skipped.extend(import_report.skipped);
        report
            .unresolved_endpoints
            .extend(import_report.unresolved_endpoints);
        Ok(())
    }

    fn run_edges(
        &mut self,
        path: &Path,
        opts: &Neo4jCsvOptions,
        report: &mut Neo4jFidelityReport,
    ) -> Result<()> {
        let headers = parse::read_csv_header(path, opts.delimiter, opts.quote)?;
        let columns = parse_columns(&headers).map_err(Error::other)?;

        let start_group = columns.iter().find_map(|c| match &c.kind {
            ColKind::StartId { group } => Some(group.clone()),
            _ => None,
        });
        let end_group = columns.iter().find_map(|c| match &c.kind {
            ColKind::EndId { group } => Some(group.clone()),
            _ => None,
        });
        let start_group = match start_group {
            Some(g) => g,
            None => {
                return Err(Error::other(
                    "relationship file has no :START_ID column".to_string(),
                ));
            }
        };
        let end_group = match end_group {
            Some(g) => g,
            None => {
                return Err(Error::other(
                    "relationship file has no :END_ID column".to_string(),
                ));
            }
        };
        if !columns.iter().any(|c| matches!(c.kind, ColKind::Type)) {
            return Err(Error::other(
                "relationship file has no :TYPE column".to_string(),
            ));
        }
        if opts.strict_types {
            enforce_strict_types(&columns).map_err(Error::other)?;
        }

        let rows = parse::csv_rows_with(path, opts.delimiter, opts.quote)?;
        let prev_prov = self.config.provenance.take();
        self.config.provenance = Some(provenance_for(path)?);

        let columns_ref = &columns;
        let start_group_ref = start_group.as_deref();
        let end_group_ref = end_group.as_deref();
        let result = self.import_edges_with(rows, |key_to_id, row| {
            prepare_edge(
                key_to_id,
                row,
                columns_ref,
                start_group_ref,
                end_group_ref,
                opts,
                report,
            )
        });

        self.config.provenance = prev_prov;
        let import_report = result?;

        report.rows_read += import_report.rows_read;
        report.relationships_imported += import_report.edges_imported;
        report.skipped.extend(import_report.skipped);
        report
            .unresolved_endpoints
            .extend(import_report.unresolved_endpoints);
        Ok(())
    }
}
