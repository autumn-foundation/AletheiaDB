//! Bulk graph importer for CSV/JSONL with per-row valid-time backfill (Issue #3211).
//!
//! The importer loads a file of nodes and a file of edges into the graph in one pass.
//! Compared to a hand-written per-record `create_node` / `create_edge` loop it:
//!
//! - commits **chunks** of rows in a single ACID transaction (never one commit per row),
//!   routing the writes through the WAL `append_batch` path for higher throughput;
//! - supports an **optional per-row `valid_time` column**, so historical facts are
//!   backfilled and immediately queryable with `AS OF VALID_TIME`. When the column is
//!   absent (or blank for a row) the row defaults to import (transaction) time;
//! - resolves edges by a user-supplied **business key** (not internal `NodeId`) in one
//!   pass; unresolved endpoints are **reported, not silently dropped**;
//! - reports malformed rows with a precise `row N:` message, with a caller-selectable
//!   [`FailureMode`] (abort on the first bad row, or skip-and-report).
//!
//! # Atomicity
//!
//! Each chunk of [`ImportConfig::batch_size`] rows commits as one ACID transaction. A
//! multi-file import (nodes then edges) is therefore **not** a single global transaction:
//! in [`FailureMode::Abort`] the first bad row returns an `Err` and no further chunks are
//! committed, but any *already-committed* chunks persist. Callers needing all-or-nothing
//! semantics should validate input first or run against a disposable database.
//!
//! # Example
//!
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::api::import::{ColumnType, EdgeMapping, LabelSource, NodeMapping};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let mut importer = db.import();
//!
//! let nodes = NodeMapping::new(LabelSource::fixed("Person"), "id")
//!     .property("name", "name", ColumnType::String)
//!     .valid_time_column("known_since");
//! importer.nodes_from_csv("people.csv", nodes)?;
//!
//! let edges = EdgeMapping::new(LabelSource::fixed("KNOWS"), "src", "dst");
//! importer.edges_from_csv("knows.csv", edges)?;
//! # Ok(())
//! # }
//! ```

mod mapping;
mod neo4j;
mod parse;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod neo4j_tests;

pub use mapping::{ColumnType, EdgeMapping, LabelSource, NodeMapping, PropertyMapping};
pub use neo4j::{
    CoercionNote, LabelMapEntry, LabelStrategy, Neo4jCsvOptions, Neo4jFidelityReport, TypeMapEntry,
    UnsupportedNote,
};

use std::collections::HashMap;
use std::fmt;
use std::path::Path;

use parse::{Row, RowIter};

use crate::core::error::{Error, Result};
use crate::core::id::NodeId;
use crate::core::property::{PropertyMap, PropertyMapBuilder, PropertyValue};
use crate::core::provenance::Provenance;
use crate::core::temporal::Timestamp;
use crate::db::AletheiaDB;

/// How the importer reacts to a malformed row or an edge whose endpoint key cannot
/// be resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FailureMode {
    /// Abort the entire import on the first bad row, returning an `Err`.
    #[default]
    Abort,
    /// Skip bad rows, collect them in the [`ImportReport`], and keep importing.
    SkipAndReport,
}

/// Tunables for an import run.
#[derive(Debug, Clone)]
pub struct ImportConfig {
    /// Number of rows committed per WAL batch transaction (default `1000`).
    pub batch_size: usize,
    /// How to react to malformed rows / unresolved endpoints (default [`FailureMode::Abort`]).
    pub failure_mode: FailureMode,
    /// Optional write-time [`Provenance`] bundle stamped on every imported
    /// node/edge version (Issue #3224). When `None` (the default) the import
    /// records no provenance, reproducing the historical behavior of the
    /// CSV/JSONL importers exactly. The Neo4j importer (Issue #3356) sets this
    /// to `neo4j-import::<file>`.
    pub provenance: Option<Provenance>,
}

impl Default for ImportConfig {
    fn default() -> Self {
        ImportConfig {
            batch_size: 1000,
            failure_mode: FailureMode::Abort,
            provenance: None,
        }
    }
}

/// Which end of an edge failed to resolve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum Endpoint {
    /// The edge's source endpoint.
    Source,
    /// The edge's target endpoint.
    Target,
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Endpoint::Source => f.write_str("source"),
            Endpoint::Target => f.write_str("target"),
        }
    }
}

/// A row that could not be imported, with its 1-based row/line number.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RowError {
    /// The 1-based row (CSV) or line (JSONL) number.
    pub row: usize,
    /// A precise, human-readable reason.
    pub message: String,
}

impl fmt::Display for RowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "row {}: {}", self.row, self.message)
    }
}

/// An edge whose source or target business key did not resolve to an imported node.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct UnresolvedEndpoint {
    /// The 1-based row number of the offending edge.
    pub row: usize,
    /// The unresolved business key.
    pub key: String,
    /// Which endpoint failed to resolve.
    pub side: Endpoint,
}

impl fmt::Display for UnresolvedEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "row {}: unresolved {} key '{}'",
            self.row, self.side, self.key
        )
    }
}

/// Summary of a single `nodes_from_*` / `edges_from_*` call.
#[derive(Debug, Clone, Default)]
pub struct ImportReport {
    /// Number of input rows read (including skipped ones).
    pub rows_read: usize,
    /// Number of nodes successfully imported.
    pub nodes_imported: usize,
    /// Number of edges successfully imported.
    pub edges_imported: usize,
    /// Malformed rows that were skipped (only populated in [`FailureMode::SkipAndReport`]).
    pub skipped: Vec<RowError>,
    /// Edges whose endpoints could not be resolved (only populated in
    /// [`FailureMode::SkipAndReport`]).
    pub unresolved_endpoints: Vec<UnresolvedEndpoint>,
}

/// Errors raised by the bulk importer.
#[derive(Debug)]
pub enum ImportError {
    /// A malformed row.
    Row(RowError),
    /// An edge endpoint that did not resolve to a node.
    UnresolvedEndpoint(UnresolvedEndpoint),
    /// An I/O error opening or reading the source file.
    Io(String),
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImportError::Row(e) => write!(f, "{e}"),
            ImportError::UnresolvedEndpoint(e) => write!(f, "{e}"),
            ImportError::Io(msg) => write!(f, "import I/O error: {msg}"),
        }
    }
}

impl std::error::Error for ImportError {}

impl From<ImportError> for Error {
    fn from(e: ImportError) -> Self {
        Error::other(e.to_string())
    }
}

/// Internal error from preparing a single edge: either a malformed row or an
/// unresolved endpoint.
enum EdgePrepError {
    Row(RowError),
    Unresolved(UnresolvedEndpoint),
}

/// A node prepared (parsed + coerced) and ready to be written.
struct PreparedNode {
    key: String,
    label: String,
    properties: PropertyMap,
    valid_from: Option<Timestamp>,
}

/// An edge prepared (parsed + coerced + endpoints resolved) and ready to be written.
struct PreparedEdge {
    source: NodeId,
    target: NodeId,
    label: String,
    properties: PropertyMap,
    valid_from: Option<Timestamp>,
}

/// A stateful bulk importer obtained via [`AletheiaDB::import`].
///
/// The importer remembers the business-key → [`NodeId`] mapping built while importing
/// nodes, so a subsequent `edges_from_*` call can resolve edge endpoints by key.
pub struct Importer<'a> {
    db: &'a AletheiaDB,
    config: ImportConfig,
    key_to_id: HashMap<String, NodeId>,
}

impl AletheiaDB {
    /// Begin a bulk import session.
    ///
    /// Returns a stateful [`Importer`] that resolves edge endpoints against nodes
    /// imported earlier in the same session. See the [module docs](self) for details.
    pub fn import(&self) -> Importer<'_> {
        Importer {
            db: self,
            config: ImportConfig::default(),
            key_to_id: HashMap::new(),
        }
    }
}

impl<'a> Importer<'a> {
    /// Override the full import configuration.
    #[must_use]
    pub fn with_config(mut self, config: ImportConfig) -> Self {
        self.config = config;
        self
    }

    /// Set the failure mode (abort vs. skip-and-report).
    #[must_use]
    pub fn failure_mode(mut self, mode: FailureMode) -> Self {
        self.config.failure_mode = mode;
        self
    }

    /// Set the per-transaction batch size.
    #[must_use]
    pub fn batch_size(mut self, batch_size: usize) -> Self {
        self.config.batch_size = batch_size.max(1);
        self
    }

    /// Look up the [`NodeId`] imported for a business key (handy for assertions/examples).
    pub fn resolve_key(&self, key: &str) -> Option<NodeId> {
        self.key_to_id.get(key).copied()
    }

    // ---- Nodes -----------------------------------------------------------------

    /// Import nodes from a CSV file.
    pub fn nodes_from_csv(
        &mut self,
        path: impl AsRef<Path>,
        mapping: NodeMapping,
    ) -> Result<ImportReport> {
        let rows = parse::csv_rows(path.as_ref())?;
        self.import_nodes(rows, &mapping)
    }

    /// Import nodes from a JSONL file (one JSON object per line).
    pub fn nodes_from_jsonl(
        &mut self,
        path: impl AsRef<Path>,
        mapping: NodeMapping,
    ) -> Result<ImportReport> {
        let rows = parse::jsonl_rows(path.as_ref())?;
        self.import_nodes(rows, &mapping)
    }

    fn import_nodes(&mut self, rows: RowIter, mapping: &NodeMapping) -> Result<ImportReport> {
        self.import_nodes_with(rows, |row| prepare_node(row, mapping))
    }

    /// Shared node-import driver: chunked ACID commit + failure-mode handling,
    /// parameterized by a per-row `prepare` closure that turns a raw [`Row`]
    /// into a [`PreparedNode`]. Both the CSV/JSONL importers (Issue #3211) and
    /// the Neo4j importer (Issue #3356) route through this so the commit path,
    /// batching, provenance, and `row N:` error contract stay identical.
    fn import_nodes_with<F>(&mut self, rows: RowIter, mut prepare: F) -> Result<ImportReport>
    where
        F: FnMut(&Row) -> std::result::Result<PreparedNode, RowError>,
    {
        let mut report = ImportReport::default();
        let mut chunk: Vec<PreparedNode> = Vec::with_capacity(self.config.batch_size);

        for row_result in rows {
            report.rows_read += 1;
            let row = match row_result {
                Ok(row) => row,
                Err(err) => {
                    self.handle_row_failure(err, &mut report)?;
                    continue;
                }
            };

            match prepare(&row) {
                Ok(prepared) => chunk.push(prepared),
                Err(row_err) => match self.config.failure_mode {
                    FailureMode::Abort => return Err(ImportError::Row(row_err).into()),
                    FailureMode::SkipAndReport => report.skipped.push(row_err),
                },
            }

            if chunk.len() >= self.config.batch_size {
                self.commit_node_chunk(&mut chunk, &mut report)?;
            }
        }

        self.commit_node_chunk(&mut chunk, &mut report)?;
        Ok(report)
    }

    fn commit_node_chunk(
        &mut self,
        chunk: &mut Vec<PreparedNode>,
        report: &mut ImportReport,
    ) -> Result<()> {
        if chunk.is_empty() {
            return Ok(());
        }
        let prepared = std::mem::take(chunk);
        let keys: Vec<String> = prepared.iter().map(|p| p.key.clone()).collect();
        let provenance = self.config.provenance.clone();

        // One ACID transaction for the whole chunk: writes flow through the WAL
        // `append_batch` path (see `api::transaction::write::wal`).
        let ids = self.db.write(move |tx| {
            use crate::api::transaction::{WriteOps, WriteRequestOptions};
            let mut ids = Vec::with_capacity(prepared.len());
            for node in prepared {
                let mut options = WriteRequestOptions::new();
                if let Some(valid_from) = node.valid_from {
                    options = options.with_valid_from(valid_from);
                }
                if let Some(provenance) = provenance.clone() {
                    options = options.with_provenance(provenance);
                }
                let id = tx.create_node_with_options(&node.label, node.properties, options)?;
                ids.push(id);
            }
            Ok::<Vec<NodeId>, Error>(ids)
        })?;

        for (key, id) in keys.into_iter().zip(ids) {
            self.key_to_id.insert(key, id);
            report.nodes_imported += 1;
        }
        Ok(())
    }

    // ---- Edges -----------------------------------------------------------------

    /// Import edges from a CSV file, resolving endpoints by business key.
    pub fn edges_from_csv(
        &mut self,
        path: impl AsRef<Path>,
        mapping: EdgeMapping,
    ) -> Result<ImportReport> {
        let rows = parse::csv_rows(path.as_ref())?;
        self.import_edges(rows, &mapping)
    }

    /// Import edges from a JSONL file, resolving endpoints by business key.
    pub fn edges_from_jsonl(
        &mut self,
        path: impl AsRef<Path>,
        mapping: EdgeMapping,
    ) -> Result<ImportReport> {
        let rows = parse::jsonl_rows(path.as_ref())?;
        self.import_edges(rows, &mapping)
    }

    fn import_edges(&mut self, rows: RowIter, mapping: &EdgeMapping) -> Result<ImportReport> {
        self.import_edges_with(rows, |key_to_id, row| prepare_edge(key_to_id, row, mapping))
    }

    /// Shared edge-import driver: the edge counterpart to [`import_nodes_with`].
    /// The `prepare` closure receives the accumulated business-key → [`NodeId`]
    /// map so endpoints resolve against nodes imported earlier in the session.
    fn import_edges_with<F>(&mut self, rows: RowIter, mut prepare: F) -> Result<ImportReport>
    where
        F: FnMut(
            &HashMap<String, NodeId>,
            &Row,
        ) -> std::result::Result<PreparedEdge, EdgePrepError>,
    {
        let mut report = ImportReport::default();
        let mut chunk: Vec<PreparedEdge> = Vec::with_capacity(self.config.batch_size);

        for row_result in rows {
            report.rows_read += 1;
            let row = match row_result {
                Ok(row) => row,
                Err(err) => {
                    self.handle_row_failure(err, &mut report)?;
                    continue;
                }
            };

            match prepare(&self.key_to_id, &row) {
                Ok(prepared) => chunk.push(prepared),
                Err(EdgePrepError::Row(row_err)) => match self.config.failure_mode {
                    FailureMode::Abort => return Err(ImportError::Row(row_err).into()),
                    FailureMode::SkipAndReport => report.skipped.push(row_err),
                },
                Err(EdgePrepError::Unresolved(endpoint)) => match self.config.failure_mode {
                    FailureMode::Abort => {
                        return Err(ImportError::UnresolvedEndpoint(endpoint).into());
                    }
                    FailureMode::SkipAndReport => report.unresolved_endpoints.push(endpoint),
                },
            }

            if chunk.len() >= self.config.batch_size {
                self.commit_edge_chunk(&mut chunk, &mut report)?;
            }
        }

        self.commit_edge_chunk(&mut chunk, &mut report)?;
        Ok(report)
    }

    fn commit_edge_chunk(
        &mut self,
        chunk: &mut Vec<PreparedEdge>,
        report: &mut ImportReport,
    ) -> Result<()> {
        if chunk.is_empty() {
            return Ok(());
        }
        let prepared = std::mem::take(chunk);
        let count = prepared.len();
        let provenance = self.config.provenance.clone();

        self.db.write(move |tx| {
            use crate::api::transaction::{WriteOps, WriteRequestOptions};
            for edge in prepared {
                let mut options = WriteRequestOptions::new();
                if let Some(valid_from) = edge.valid_from {
                    options = options.with_valid_from(valid_from);
                }
                if let Some(provenance) = provenance.clone() {
                    options = options.with_provenance(provenance);
                }
                tx.create_edge_with_options(
                    edge.source,
                    edge.target,
                    &edge.label,
                    edge.properties,
                    options,
                )?;
            }
            Ok::<(), Error>(())
        })?;

        report.edges_imported += count;
        Ok(())
    }

    /// Apply the failure mode to a row-level reader/parse error.
    fn handle_row_failure(&self, err: ImportError, report: &mut ImportReport) -> Result<()> {
        match self.config.failure_mode {
            FailureMode::Abort => Err(err.into()),
            FailureMode::SkipAndReport => {
                match err {
                    ImportError::Row(row_err) => report.skipped.push(row_err),
                    ImportError::UnresolvedEndpoint(endpoint) => {
                        report.unresolved_endpoints.push(endpoint)
                    }
                    // An I/O error mid-stream is fatal even in skip mode.
                    ImportError::Io(_) => return Err(err.into()),
                }
                Ok(())
            }
        }
    }
}

/// Resolve a label from a row per the [`LabelSource`].
fn resolve_label(row: &Row, source: &LabelSource) -> std::result::Result<String, RowError> {
    match source {
        LabelSource::Fixed(label) => Ok(label.clone()),
        LabelSource::Column(column) => {
            let value = row.get_str(column).map_err(|message| RowError {
                row: row.index,
                message,
            })?;
            if value.trim().is_empty() {
                return Err(RowError {
                    row: row.index,
                    message: format!("label column '{column}' is empty"),
                });
            }
            Ok(value)
        }
    }
}

/// Build a [`PropertyMap`] from the mapping's property columns.
fn build_properties(
    row: &Row,
    properties: &[PropertyMapping],
) -> std::result::Result<PropertyMap, RowError> {
    let mut builder = PropertyMapBuilder::new();
    for mapping in properties {
        let cell = row.cells.get(&mapping.column).ok_or_else(|| RowError {
            row: row.index,
            message: format!("missing column '{}'", mapping.column),
        })?;
        let value = parse::coerce(cell, mapping.ty).map_err(|message| RowError {
            row: row.index,
            message: format!("column '{}': {message}", mapping.column),
        })?;
        // Skip nulls so blank cells become absent properties rather than stored nulls.
        if matches!(value, PropertyValue::Null) {
            continue;
        }
        builder = builder
            .try_insert(&mapping.name, value)
            .map_err(|e| RowError {
                row: row.index,
                message: e.to_string(),
            })?;
    }
    Ok(builder.build())
}

/// Extract an optional valid time from the named column.
fn extract_valid_time(
    row: &Row,
    column: &Option<String>,
) -> std::result::Result<Option<Timestamp>, RowError> {
    let Some(column) = column else {
        return Ok(None);
    };
    let raw = row.get_str(column).map_err(|message| RowError {
        row: row.index,
        message,
    })?;
    if raw.trim().is_empty() {
        return Ok(None);
    }
    let ts = parse::parse_valid_time(&raw).map_err(|message| RowError {
        row: row.index,
        message,
    })?;
    Ok(Some(ts))
}

fn prepare_node(row: &Row, mapping: &NodeMapping) -> std::result::Result<PreparedNode, RowError> {
    let label = resolve_label(row, &mapping.label)?;
    let key = row
        .get_str(&mapping.key_column)
        .map_err(|message| RowError {
            row: row.index,
            message,
        })?;
    if key.trim().is_empty() {
        return Err(RowError {
            row: row.index,
            message: format!("key column '{}' is empty", mapping.key_column),
        });
    }
    let properties = build_properties(row, &mapping.properties)?;
    let valid_from = extract_valid_time(row, &mapping.valid_time_column)?;
    Ok(PreparedNode {
        key,
        label,
        properties,
        valid_from,
    })
}

fn prepare_edge(
    key_to_id: &HashMap<String, NodeId>,
    row: &Row,
    mapping: &EdgeMapping,
) -> std::result::Result<PreparedEdge, EdgePrepError> {
    let label = resolve_label(row, &mapping.label).map_err(EdgePrepError::Row)?;

    let source_key = row.get_str(&mapping.source_key_column).map_err(|message| {
        EdgePrepError::Row(RowError {
            row: row.index,
            message,
        })
    })?;
    let target_key = row.get_str(&mapping.target_key_column).map_err(|message| {
        EdgePrepError::Row(RowError {
            row: row.index,
            message,
        })
    })?;

    let source = key_to_id.get(&source_key).copied().ok_or_else(|| {
        EdgePrepError::Unresolved(UnresolvedEndpoint {
            row: row.index,
            key: source_key.clone(),
            side: Endpoint::Source,
        })
    })?;
    let target = key_to_id.get(&target_key).copied().ok_or_else(|| {
        EdgePrepError::Unresolved(UnresolvedEndpoint {
            row: row.index,
            key: target_key.clone(),
            side: Endpoint::Target,
        })
    })?;

    let properties = build_properties(row, &mapping.properties).map_err(EdgePrepError::Row)?;
    let valid_from =
        extract_valid_time(row, &mapping.valid_time_column).map_err(EdgePrepError::Row)?;

    Ok(PreparedEdge {
        source,
        target,
        label,
        properties,
        valid_from,
    })
}
