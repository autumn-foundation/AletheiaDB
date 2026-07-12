//! Columnar Parquet export of the graph for analytics interoperability (Issue #3364).
//!
//! This is the export half of the Parquet import/export layer; the import half lives
//! in [`crate::api::import`] and shares the same columnar schema so that an
//! export → import round-trip reproduces the graph. Two modes are offered:
//!
//! - **Current-state export** ([`Exporter::nodes_to_parquet`] /
//!   [`Exporter::edges_to_parquet`]) writes the live graph with a stable, documented
//!   schema: an `id`/`label` (or `edge_type`/`source_key`/`target_key`) header, a
//!   nullable `valid_time` column, and one typed column per property key discovered by
//!   scanning the database. The column layout is identical to what the importer's
//!   [`NodeMapping`](crate::api::import::NodeMapping) /
//!   [`EdgeMapping`](crate::api::import::EdgeMapping) expect, so the file re-imports
//!   losslessly for scalars, timestamps, and `list<float32>` embeddings.
//! - **Full bi-temporal history export** ([`Exporter::node_history_to_parquet`] /
//!   [`Exporter::edge_history_to_parquet`]) writes **one row per version** with
//!   `valid_from`, `valid_to` (**null = open interval**, never a sentinel),
//!   `transaction_time`, a `tombstone` marker, and the five provenance columns.
//!
//! # Streaming and memory
//!
//! Both modes stream: the database is scanned once to discover the property schema,
//! then a second time to emit rows in [`ExportConfig::batch_size`]-row Arrow
//! `RecordBatch`es, each flushed as a Parquet row group before the next is built. Peak
//! memory is therefore bounded to roughly one batch of decoded cells plus the set of
//! distinct property keys, not the whole graph (target: 1M nodes in well under 1 GB).
//!
//! # Documented lossy items
//!
//! - The HLC **logical counter** (the sub-microsecond tiebreaker of a
//!   [`HybridTimestamp`](crate::core::hlc::HybridTimestamp)) is dropped; only the
//!   microsecond wallclock is written.
//! - Property values that have no natural columnar type — [`PropertyValue::Bytes`],
//!   heterogeneous [`PropertyValue::Array`], and [`PropertyValue::SparseVector`] — and
//!   any property **key whose observed type conflicts across rows** are routed to a
//!   single `properties_json` overflow column (a reversible tagged-JSON encoding), not
//!   a native typed column. The default importer decodes native columns; re-importing
//!   the overflow column requires custom mapping (see the guide).
//! - History export enumerates entities through the whole-window **changefeed**, so an
//!   entity that was deleted or retracted (and is no longer in current state) is still
//!   exported with its full version history and tombstone. A deleted **edge**'s
//!   endpoints are not carried on a historical version, so `source_key`/`target_key` are
//!   written as null for an edge that is no longer live; the residual completeness caveat
//!   is the changefeed's own (see the guide).

#[cfg(feature = "parquet")]
mod parquet;

#[cfg(all(test, feature = "parquet"))]
mod tests;

use std::path::Path;

use crate::core::error::Result;
use crate::db::AletheiaDB;

/// Tunables for an export run.
#[derive(Debug, Clone)]
pub struct ExportConfig {
    /// Number of rows per Arrow `RecordBatch` / Parquet row group (default `8192`).
    ///
    /// Bounds peak export memory: a larger batch amortizes write overhead, a smaller
    /// batch lowers the memory high-water mark. Clamped to at least `1`.
    pub batch_size: usize,
}

impl Default for ExportConfig {
    fn default() -> Self {
        ExportConfig { batch_size: 8192 }
    }
}

/// Summary of a single `*_to_parquet` call.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExportReport {
    /// Number of current-state nodes written (current-state node export).
    pub nodes_exported: usize,
    /// Number of current-state edges written (current-state edge export).
    pub edges_exported: usize,
    /// Number of node **versions** written (history node export).
    pub node_versions_exported: usize,
    /// Number of edge **versions** written (history edge export).
    pub edge_versions_exported: usize,
}

/// A stateless Parquet exporter obtained via [`AletheiaDB::export`].
pub struct Exporter<'a> {
    db: &'a AletheiaDB,
    config: ExportConfig,
}

impl AletheiaDB {
    /// Begin a Parquet export session.
    ///
    /// Returns an [`Exporter`] bound to this database. See the [module docs](self) for
    /// the schema, streaming behavior, and documented lossy items.
    pub fn export(&self) -> Exporter<'_> {
        Exporter {
            db: self,
            config: ExportConfig::default(),
        }
    }
}

impl<'a> Exporter<'a> {
    /// Override the full export configuration.
    #[must_use]
    pub fn with_config(mut self, config: ExportConfig) -> Self {
        self.config = config;
        self
    }

    /// Set the per-row-group batch size (clamped to at least `1`).
    #[must_use]
    pub fn batch_size(mut self, batch_size: usize) -> Self {
        self.config.batch_size = batch_size.max(1);
        self
    }

    /// Export the current state of every node to a Parquet file (Issue #3364).
    ///
    /// Columns: `id: int64`, `label: string`, `valid_time: timestamp(µs, UTC)`
    /// (nullable), then one typed column per property key (scalars → int64 / double /
    /// bool / string, embeddings → `list<float32>`), plus a `properties_json` overflow
    /// column when any key needs it. See the [module docs](self).
    #[cfg(feature = "parquet")]
    pub fn nodes_to_parquet(&self, path: impl AsRef<Path>) -> Result<ExportReport> {
        parquet::export_nodes(self.db, path.as_ref(), self.config.batch_size.max(1))
    }

    /// Export the current state of every edge to a Parquet file (Issue #3364).
    ///
    /// Columns: `edge_type: string`, `source_key: int64`, `target_key: int64`,
    /// `valid_time: timestamp(µs, UTC)` (nullable), then one typed column per property
    /// key plus the `properties_json` overflow column when needed. Endpoint keys are
    /// the endpoints' node ids, matching the `id` column of a node export.
    #[cfg(feature = "parquet")]
    pub fn edges_to_parquet(&self, path: impl AsRef<Path>) -> Result<ExportReport> {
        parquet::export_edges(self.db, path.as_ref(), self.config.batch_size.max(1))
    }

    /// Export the full bi-temporal history of every node — one row per version.
    ///
    /// Columns: `id: int64`, `label: string`, `version: int64`, the typed property
    /// columns, `valid_from`/`valid_to` (`valid_to` null = still-open interval),
    /// `transaction_time`, `tombstone: bool`, and the five nullable `provenance_*`
    /// columns. See the [module docs](self) for the tombstone convention and the
    /// deleted-edge enumeration caveat (which applies to
    /// [`edge_history_to_parquet`](Self::edge_history_to_parquet)).
    #[cfg(feature = "parquet")]
    pub fn node_history_to_parquet(&self, path: impl AsRef<Path>) -> Result<ExportReport> {
        parquet::export_node_history(self.db, path.as_ref(), self.config.batch_size.max(1))
    }

    /// Export the full bi-temporal history of every edge — one row per version.
    ///
    /// Same shape as [`node_history_to_parquet`](Self::node_history_to_parquet) plus
    /// `edge_type`/`source_key`/`target_key`. Edges are enumerated through
    /// current-state adjacency; see the [module docs](self) for the deleted-edge gap.
    #[cfg(feature = "parquet")]
    pub fn edge_history_to_parquet(&self, path: impl AsRef<Path>) -> Result<ExportReport> {
        parquet::export_edge_history(self.db, path.as_ref(), self.config.batch_size.max(1))
    }
}
