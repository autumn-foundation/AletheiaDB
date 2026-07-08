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

/// Resolve an interned label/key to an owned `String`, falling back to empty
/// if the interner entry is somehow missing (cannot happen in practice,
/// since labels and property keys are interned before being stored on a
/// node/edge).
fn resolve_label(label: InternedString) -> String {
    GLOBAL_INTERNER.resolve_or_else(label, String::new)
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
/// [`AletheiaDB::schema_as_of`] (bi-temporal). Property-key enumeration
/// within a summarized label/type is always exhaustive; `sampled` instead
/// reports whether the *set of entities summarized* was truncated (see
/// [`AletheiaDB::schema_as_of`]'s entity cap) -- any such truncation is
/// always disclosed via this flag, never silent.
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
    /// Whether the summarized entity set was truncated (see
    /// [`AletheiaDB::schema_as_of`]'s entity cap). Always `false` for
    /// [`AletheiaDB::schema`], which is always exhaustive.
    pub sampled: bool,
    /// The bi-temporal instant this schema reflects, or `None` for current state.
    pub as_of: Option<SchemaInstant>,
}

/// Accumulates per-label/per-type counts and the union of property keys
/// seen, keyed by `InternedString` rather than resolving to `String` per
/// entity -- resolution happens once per *distinct* label/key in
/// [`build_schema`] instead of once per node/edge, avoiding an interner
/// lookup + allocation for every property of every entity.
struct Accumulator {
    entries: BTreeMap<InternedString, (usize, BTreeSet<InternedString>)>,
}

impl Accumulator {
    fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
        }
    }

    fn record(&mut self, label: InternedString, properties: &PropertyMap) {
        let entry = self
            .entries
            .entry(label)
            .or_insert_with(|| (0, BTreeSet::new()));
        entry.0 += 1;
        entry.1.extend(properties.keys().copied());
    }
}

impl AletheiaDB {
    /// Discover the graph's current schema: distinct node labels and edge
    /// types, their counts, and the property keys observed on each.
    ///
    /// Returns an empty-but-well-formed [`GraphSchema`] on an empty database,
    /// never an error, and is always exhaustive (`sampled` is always
    /// `false`). Counts reflect a best-effort read over live storage: as
    /// with every other full-scan method on this storage layer (e.g.
    /// `label_counts`), there is no cross-call snapshot isolation, so under
    /// concurrent writes the totals may not exactly match a `node_count`/
    /// `edge_count` call made at a slightly different instant.
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
        self.current
            .visit_nodes(|node| node_acc.record(node.label, &node.properties));

        let mut edge_acc = Accumulator::new();
        self.current
            .visit_edges(|edge| edge_acc.record(edge.label, &edge.properties));

        Ok(build_schema(node_acc, edge_acc, false, None))
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
    /// The set of candidate entities (every node/edge that has ever had a
    /// version recorded) is capped per entity kind (see
    /// [`crate::config::HistoricalConfigBuilder::max_schema_as_of_entities`],
    /// default 50,000) to keep this call bounded on databases with
    /// substantial bi-temporal history; when the cap is hit,
    /// [`GraphSchema::sampled`] is set to `true` to disclose the truncation.
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
        let (node_ids, edge_ids, sampled) = {
            let historical = self.historical.read();
            let mut node_ids = historical.versioned_node_ids();
            let mut edge_ids = historical.versioned_edge_ids();
            let cap = historical.max_schema_as_of_entities();
            drop(historical);

            let node_sampled = cap_ids(&mut node_ids, cap);
            let edge_sampled = cap_ids(&mut edge_ids, cap);
            (node_ids, edge_ids, node_sampled || edge_sampled)
        };

        let mut node_acc = Accumulator::new();
        for (_, node) in self.get_nodes_at_time(&node_ids, valid_time, transaction_time)? {
            if let Some(node) = node {
                node_acc.record(node.label, &node.properties);
            }
        }

        let mut edge_acc = Accumulator::new();
        for (_, edge) in self.get_edges_at_time(&edge_ids, valid_time, transaction_time)? {
            if let Some(edge) = edge {
                edge_acc.record(edge.label, &edge.properties);
            }
        }

        Ok(build_schema(
            node_acc,
            edge_acc,
            sampled,
            Some(SchemaInstant {
                valid_time,
                transaction_time,
            }),
        ))
    }
}

/// Truncate `ids` down to the `cap` smallest elements (a deterministic set,
/// though not necessarily in sorted order -- the order among the kept
/// elements doesn't matter, since they're only used as a scan input, not
/// exposed to callers). Returns `true` if truncation actually happened, so
/// the caller can disclose it via [`GraphSchema::sampled`] (or
/// [`crate::db::NodesAtTime::sampled`] on the AS OF node-find path, which
/// shares this cap).
///
/// Uses a partial selection (`select_nth_unstable`, O(n) average) rather
/// than a full sort (O(n log n)), since only membership in the smallest-`cap`
/// set matters here, not a total order.
pub(crate) fn cap_ids<T: Ord>(ids: &mut Vec<T>, cap: usize) -> bool {
    if ids.len() > cap {
        ids.select_nth_unstable(cap);
        ids.truncate(cap);
        true
    } else {
        false
    }
}

fn build_schema(
    node_acc: Accumulator,
    edge_acc: Accumulator,
    sampled: bool,
    as_of: Option<SchemaInstant>,
) -> GraphSchema {
    let mut total_nodes = 0;
    let mut node_labels: Vec<LabelSchema> = node_acc
        .entries
        .into_iter()
        .map(|(label, (count, keys))| {
            total_nodes += count;
            let mut property_keys: Vec<String> = keys.into_iter().map(resolve_label).collect();
            property_keys.sort_unstable();
            LabelSchema {
                label: resolve_label(label),
                count,
                property_keys,
            }
        })
        .collect();
    node_labels.sort_unstable_by(|a, b| a.label.cmp(&b.label));

    let mut total_edges = 0;
    let mut edge_types: Vec<EdgeTypeSchema> = edge_acc
        .entries
        .into_iter()
        .map(|(edge_type, (count, keys))| {
            total_edges += count;
            let mut property_keys: Vec<String> = keys.into_iter().map(resolve_label).collect();
            property_keys.sort_unstable();
            EdgeTypeSchema {
                edge_type: resolve_label(edge_type),
                count,
                property_keys,
            }
        })
        .collect();
    edge_types.sort_unstable_by(|a, b| a.edge_type.cmp(&b.edge_type));

    GraphSchema {
        node_labels,
        edge_types,
        total_nodes,
        total_edges,
        sampled,
        as_of,
    }
}

#[cfg(test)]
mod cap_ids_tests {
    use super::cap_ids;

    #[test]
    fn under_cap_is_untouched_and_not_sampled() {
        let mut ids = vec![5, 1, 3];
        let sampled = cap_ids(&mut ids, 10);
        assert!(!sampled);
        assert_eq!(ids, vec![5, 1, 3], "no truncation, order preserved");
    }

    #[test]
    fn exactly_at_cap_is_untouched_and_not_sampled() {
        let mut ids = vec![5, 1, 3];
        let sampled = cap_ids(&mut ids, 3);
        assert!(!sampled);
        assert_eq!(ids, vec![5, 1, 3]);
    }

    #[test]
    fn over_cap_truncates_to_the_cap_smallest_elements_and_is_sampled() {
        let mut ids = vec![9, 1, 5, 3, 7, 2];
        let sampled = cap_ids(&mut ids, 3);
        assert!(sampled);
        assert_eq!(ids.len(), 3);
        // select_nth_unstable only guarantees a correct partition, not a
        // sorted order among the kept elements -- compare as a set.
        let mut kept = ids.clone();
        kept.sort_unstable();
        assert_eq!(kept, vec![1, 2, 3], "kept elements are the 3 smallest");
    }

    #[test]
    fn cap_of_zero_truncates_everything() {
        let mut ids = vec![1, 2, 3];
        let sampled = cap_ids(&mut ids, 0);
        assert!(sampled);
        assert!(ids.is_empty());
    }
}
