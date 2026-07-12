//! Column mappings that tell the [`Importer`](super::Importer) how to interpret
//! the rows of a CSV/JSONL file when building graph nodes and edges.
//!
//! Mappings are explicit: the caller declares which column supplies the label, the
//! business key, each property (with a coercion [`ColumnType`]), and — optionally —
//! the per-row `valid_time`. There is no schema inference (out of scope for #3211).

/// The type a source column's raw value is coerced into before being stored as a
/// property value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnType {
    /// Store the value as a UTF-8 string.
    String,
    /// Parse the value as a 64-bit signed integer.
    Int,
    /// Parse the value as a 64-bit float.
    Float,
    /// Parse the value as a boolean (`true`/`false`, case-insensitive, or `1`/`0`).
    Bool,
    /// A timestamp column stored as microseconds since the Unix epoch.
    ///
    /// Text/JSON inputs accept the same formats as the per-row `valid_time` column
    /// (RFC 3339 / ISO 8601, a bare date, or bare integer microseconds); a native
    /// Parquet `timestamp` column is decoded to microseconds without a string
    /// round-trip. The value is stored as [`PropertyValue::Int`](crate::core::property::PropertyValue::Int)
    /// microseconds since the epoch (the property model has no dedicated timestamp
    /// variant).
    Timestamp,
    /// A dense embedding column stored as a
    /// [`PropertyValue::Vector`](crate::core::property::PropertyValue::Vector).
    ///
    /// A native Parquet `list<float32>` column is decoded to the exact `f32` bits
    /// with no string round-trip; a JSON array of numbers is also accepted.
    Embedding,
}

/// Where a node's or edge's label comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LabelSource {
    /// A constant label applied to every imported row.
    Fixed(String),
    /// The label is read from the named column on each row.
    Column(String),
}

impl LabelSource {
    /// A constant label applied to every row.
    pub fn fixed(label: impl Into<String>) -> Self {
        LabelSource::Fixed(label.into())
    }

    /// Read the label from the named column.
    pub fn column(column: impl Into<String>) -> Self {
        LabelSource::Column(column.into())
    }
}

/// Maps one source column to a stored property with an explicit coercion type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PropertyMapping {
    /// The source column name (CSV header or JSONL object key).
    pub column: String,
    /// The property name to store on the node/edge.
    pub name: String,
    /// How to coerce the raw value.
    pub ty: ColumnType,
}

impl PropertyMapping {
    /// Map `column` to a property named `name` with coercion `ty`.
    pub fn new(column: impl Into<String>, name: impl Into<String>, ty: ColumnType) -> Self {
        PropertyMapping {
            column: column.into(),
            name: name.into(),
            ty,
        }
    }

    /// Map a column to a property of the same name.
    pub fn same(column: impl Into<String>, ty: ColumnType) -> Self {
        let column = column.into();
        PropertyMapping {
            name: column.clone(),
            column,
            ty,
        }
    }
}

/// Describes how to turn the rows of a node file into graph nodes.
///
/// Built fluently:
///
/// ```rust,no_run
/// use aletheiadb::api::import::{ColumnType, LabelSource, NodeMapping};
///
/// let mapping = NodeMapping::new(LabelSource::fixed("Person"), "id")
///     .property("name", "name", ColumnType::String)
///     .property("age", "age", ColumnType::Int)
///     .valid_time_column("known_since");
/// ```
#[derive(Debug, Clone)]
pub struct NodeMapping {
    /// Where the node label comes from.
    pub label: LabelSource,
    /// The column holding the business key used to resolve edges to this node.
    pub key_column: String,
    /// Columns to store as properties.
    pub properties: Vec<PropertyMapping>,
    /// Optional column carrying the per-row valid time. When `None` (or empty for a
    /// given row) the row defaults to import (transaction) time.
    pub valid_time_column: Option<String>,
}

impl NodeMapping {
    /// Create a node mapping with the given label source and business-key column.
    pub fn new(label: LabelSource, key_column: impl Into<String>) -> Self {
        NodeMapping {
            label,
            key_column: key_column.into(),
            properties: Vec::new(),
            valid_time_column: None,
        }
    }

    /// Add a property mapping (`column` -> property `name` with coercion `ty`).
    #[must_use]
    pub fn property(
        mut self,
        column: impl Into<String>,
        name: impl Into<String>,
        ty: ColumnType,
    ) -> Self {
        self.properties.push(PropertyMapping::new(column, name, ty));
        self
    }

    /// Add a property mapping where the property name equals the column name.
    #[must_use]
    pub fn property_same(mut self, column: impl Into<String>, ty: ColumnType) -> Self {
        self.properties.push(PropertyMapping::same(column, ty));
        self
    }

    /// Designate the per-row valid-time column.
    #[must_use]
    pub fn valid_time_column(mut self, column: impl Into<String>) -> Self {
        self.valid_time_column = Some(column.into());
        self
    }
}

/// Describes how to turn the rows of an edge file into graph edges.
///
/// Edges reference nodes by a user-supplied **business key** (not internal `NodeId`);
/// the [`Importer`](super::Importer) resolves keys to ids using the nodes imported in
/// the same session.
#[derive(Debug, Clone)]
pub struct EdgeMapping {
    /// Where the edge label comes from.
    pub label: LabelSource,
    /// The column holding the source node's business key.
    pub source_key_column: String,
    /// The column holding the target node's business key.
    pub target_key_column: String,
    /// Columns to store as properties.
    pub properties: Vec<PropertyMapping>,
    /// Optional column carrying the per-row valid time.
    pub valid_time_column: Option<String>,
}

impl EdgeMapping {
    /// Create an edge mapping with the given label source and source/target key columns.
    pub fn new(
        label: LabelSource,
        source_key_column: impl Into<String>,
        target_key_column: impl Into<String>,
    ) -> Self {
        EdgeMapping {
            label,
            source_key_column: source_key_column.into(),
            target_key_column: target_key_column.into(),
            properties: Vec::new(),
            valid_time_column: None,
        }
    }

    /// Add a property mapping (`column` -> property `name` with coercion `ty`).
    #[must_use]
    pub fn property(
        mut self,
        column: impl Into<String>,
        name: impl Into<String>,
        ty: ColumnType,
    ) -> Self {
        self.properties.push(PropertyMapping::new(column, name, ty));
        self
    }

    /// Add a property mapping where the property name equals the column name.
    #[must_use]
    pub fn property_same(mut self, column: impl Into<String>, ty: ColumnType) -> Self {
        self.properties.push(PropertyMapping::same(column, ty));
        self
    }

    /// Designate the per-row valid-time column.
    #[must_use]
    pub fn valid_time_column(mut self, column: impl Into<String>) -> Self {
        self.valid_time_column = Some(column.into());
        self
    }
}
