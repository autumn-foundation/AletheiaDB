//! Graph schema discovery: node labels, edge types, and property keys.
//!
//! Provides a read-only summary of the graph's shape -- the labels and edge
//! types present, how many entities have each, and which property keys are
//! observed -- so callers (notably LLM/MCP integrators) can discover what is
//! queryable without already knowing label/property names. See
//! [`AletheiaDB::schema`] for the current-state summary and
//! [`AletheiaDB::schema_as_of`] for the bi-temporal variant.

use crate::core::error::Result;
use crate::core::interning::{GLOBAL_INTERNER, InternedString};
use crate::core::property::PropertyMap;
use crate::core::temporal::Timestamp;
use crate::db::AletheiaDB;
use std::collections::{BTreeMap, BTreeSet};

/// Resolve an interned label to an owned `String`, falling back to empty if
/// the interner entry is somehow missing (cannot happen in practice, since
/// labels are interned before being stored on a node/edge).
fn resolve_label(label: InternedString) -> String {
    GLOBAL_INTERNER
        .resolve_with(label, |s| s.to_string())
        .unwrap_or_default()
}

/// A point in bi-temporal space that a [`GraphSchema`] was computed as of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchemaInstant {
    /// The valid-time instant the schema reflects.
    pub valid_time: Timestamp,
    /// The transaction-time instant the schema reflects.
    pub transaction_time: Timestamp,
}

/// Schema summary for a single node label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelSchema {
    /// The node label.
    pub label: String,
    /// Number of nodes with this label.
    pub count: usize,
    /// Union of property keys observed on nodes with this label, sorted.
    pub property_keys: Vec<String>,
}

/// Schema summary for a single edge/relationship type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeTypeSchema {
    /// The edge/relationship type.
    pub edge_type: String,
    /// Number of edges with this type.
    pub count: usize,
    /// Union of property keys observed on edges of this type, sorted.
    pub property_keys: Vec<String>,
}

/// A structured summary of the graph's shape: distinct node labels and
/// edge/relationship types, their counts, and the property keys observed on
/// each.
///
/// Returned by [`AletheiaDB::schema`] (current state) and
/// [`AletheiaDB::schema_as_of`] (bi-temporal). Property-key enumeration is
/// always exhaustive in this version (`sampled` is always `false`); the flag
/// is present for forward compatibility should a sampling strategy be
/// introduced later -- any future sampling must set it to `true` rather than
/// silently truncating.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphSchema {
    /// Distinct node labels present, sorted by label.
    pub node_labels: Vec<LabelSchema>,
    /// Distinct edge/relationship types present, sorted by type.
    pub edge_types: Vec<EdgeTypeSchema>,
    /// Total number of nodes summarized.
    pub total_nodes: usize,
    /// Total number of edges summarized.
    pub total_edges: usize,
    /// Whether property-key enumeration was sampled rather than exhaustive.
    pub sampled: bool,
    /// The bi-temporal instant this schema reflects, or `None` for current state.
    pub as_of: Option<SchemaInstant>,
}

/// Accumulates per-label/per-type counts and the union of property keys seen.
///
/// Keyed by a `BTreeMap` so the final summary is naturally sorted by label
/// without a separate sort pass.
struct Accumulator {
    entries: BTreeMap<String, (usize, BTreeSet<String>)>,
}

impl Accumulator {
    fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
        }
    }

    fn record(&mut self, label: &str, properties: &PropertyMap) {
        let entry = self
            .entries
            .entry(label.to_string())
            .or_insert_with(|| (0, BTreeSet::new()));
        entry.0 += 1;
        entry.1.extend(
            properties
                .keys()
                .filter_map(|key| GLOBAL_INTERNER.resolve_with(*key, |s| s.to_string())),
        );
    }
}

impl AletheiaDB {
    /// Discover the graph's current schema: distinct node labels and edge
    /// types, their counts, and the property keys observed on each.
    ///
    /// Returns an empty-but-well-formed [`GraphSchema`] on an empty database,
    /// never an error. Counts are exact and match
    /// [`AletheiaDB::node_count`]/[`AletheiaDB::edge_count`] (summed across
    /// labels/types) by construction.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::AletheiaDB;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let db = AletheiaDB::new()?;
    /// let schema = db.schema()?;
    /// for label in &schema.node_labels {
    ///     println!("{}: {} nodes", label.label, label.count);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn schema(&self) -> Result<GraphSchema> {
        let mut node_acc = Accumulator::new();
        for node in self.current.get_all_nodes() {
            node_acc.record(&resolve_label(node.label), &node.properties);
        }

        let mut edge_acc = Accumulator::new();
        for edge in self.current.get_all_edges() {
            edge_acc.record(&resolve_label(edge.label), &edge.properties);
        }

        Ok(build_schema(node_acc, edge_acc, None))
    }

    /// Discover the graph's schema as it existed at a specific bi-temporal
    /// instant: the node labels and edge types visible at
    /// `(valid_time, transaction_time)`, with counts and property keys
    /// reflecting only the entities visible at that instant.
    ///
    /// A label/type that had not yet been written as of the given instant
    /// (in either temporal dimension) is simply absent from the result --
    /// there is no error case for "nothing existed yet".
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::AletheiaDB;
    /// # use aletheiadb::core::temporal::time;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let db = AletheiaDB::new()?;
    /// let now = time::now();
    /// let schema = db.schema_as_of(now, now)?;
    /// # Ok(())
    /// # }
    /// ```
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn schema_as_of(
        &self,
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Result<GraphSchema> {
        let (node_ids, edge_ids) = {
            let historical = self.historical.read();
            (
                historical.versioned_node_ids(),
                historical.versioned_edge_ids(),
            )
        };

        let mut node_acc = Accumulator::new();
        for (_, node) in self.get_nodes_at_time(&node_ids, valid_time, transaction_time)? {
            if let Some(node) = node {
                node_acc.record(&resolve_label(node.label), &node.properties);
            }
        }

        let mut edge_acc = Accumulator::new();
        for (_, edge) in self.get_edges_at_time(&edge_ids, valid_time, transaction_time)? {
            if let Some(edge) = edge {
                edge_acc.record(&resolve_label(edge.label), &edge.properties);
            }
        }

        Ok(build_schema(
            node_acc,
            edge_acc,
            Some(SchemaInstant {
                valid_time,
                transaction_time,
            }),
        ))
    }
}

fn build_schema(
    node_acc: Accumulator,
    edge_acc: Accumulator,
    as_of: Option<SchemaInstant>,
) -> GraphSchema {
    let mut total_nodes = 0;
    let node_labels: Vec<LabelSchema> = node_acc
        .entries
        .into_iter()
        .map(|(label, (count, keys))| {
            total_nodes += count;
            LabelSchema {
                label,
                count,
                property_keys: keys.into_iter().collect(),
            }
        })
        .collect();

    let mut total_edges = 0;
    let edge_types: Vec<EdgeTypeSchema> = edge_acc
        .entries
        .into_iter()
        .map(|(edge_type, (count, keys))| {
            total_edges += count;
            EdgeTypeSchema {
                edge_type,
                count,
                property_keys: keys.into_iter().collect(),
            }
        })
        .collect();

    GraphSchema {
        node_labels,
        edge_types,
        total_nodes,
        total_edges,
        sampled: false,
        as_of,
    }
}
