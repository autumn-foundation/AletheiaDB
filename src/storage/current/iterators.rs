use crate::core::id::EdgeId;
use crate::core::interning::InternedString;
use crate::index::adjacency::AdjacencyEntry;
use crate::index::incremental_adjacency::MergedAdjacencyGuard;

/// Macro to generate edge iterator types.
///
/// This eliminates code duplication between outgoing/incoming iterator variants
/// while preserving distinct types for API clarity.
macro_rules! impl_edge_iter {
    (
        $(#[$meta:meta])*
        $name:ident,
        $method_link:literal,
        $direction:literal
    ) => {
        $(#[$meta])*
        #[doc = concat!(
            "\n\nThis iterator provides zero-allocation traversal of ", $direction, " edges,",
            "\navoiding the `Vec` allocation overhead of [`", $method_link, "`](crate::storage::CurrentStorage::", $method_link, ").",
            "\n\n# Performance",
            "\n\n- **Zero allocation**: Holds a `MergedAdjacencyGuard` (merges frozen + delta)",
            "\n- **Lazy evaluation**: Only computes results as you iterate",
            "\n- **Cache-friendly**: Sequential access to CSR adjacency data",
            "\n\n# Issue #187",
            "\n\nThis iterator addresses the performance issue where edge traversal methods",
            "\nunnecessarily allocated a new `Vec<EdgeId>` on every call, adding 100-500ns",
            "\noverhead per traversal."
        )]
        pub struct $name<'a> {
            guard: MergedAdjacencyGuard<'a>,
            frozen_index: usize,
            delta_index: usize,
        }

        impl<'a> $name<'a> {
            #[doc = concat!("Create a new iterator over ", $direction, " edges.")]
            #[inline]
            pub(crate) fn new(guard: MergedAdjacencyGuard<'a>) -> Self {
                Self { guard, frozen_index: 0, delta_index: 0 }
            }
        }

        impl<'a> Iterator for $name<'a> {
            type Item = EdgeId;

            #[inline]
            fn next(&mut self) -> Option<Self::Item> {
                // Iterate frozen edges
                let frozen_slice = self.guard.frozen.get_adjacency(self.guard.node);
                while self.frozen_index < frozen_slice.len() {
                    let entry = &frozen_slice[self.frozen_index];
                    self.frozen_index += 1;
                    if self.guard.fast_path || !self.guard.tombstones.contains_key(&entry.edge_id) {
                        return Some(entry.edge_id);
                    }
                }

                // Iterate delta edges
                if let Some(delta) = &self.guard.delta {
                    while self.delta_index < delta.len() {
                        let entry = &delta[self.delta_index];
                        self.delta_index += 1;
                        if self.guard.fast_path || !self.guard.tombstones.contains_key(&entry.edge_id) {
                            return Some(entry.edge_id);
                        }
                    }
                }

                None
            }

            #[inline]
            fn size_hint(&self) -> (usize, Option<usize>) {
                // Only provide lower bound (0) as we don't know how many are tombstones
                // Calculating exact remaining count would be O(N)
                (0, None)
            }
        }

        impl<'a> std::iter::FusedIterator for $name<'a> {}
    };
}

/// Macro to generate label-filtered edge iterator types.
macro_rules! impl_edge_iter_with_label {
    (
        $(#[$meta:meta])*
        $name:ident,
        $method_link:literal,
        $direction:literal
    ) => {
        $(#[$meta])*
        #[doc = concat!(
            "\n\nThis iterator provides zero-allocation traversal of ", $direction, " edges",
            "\nthat match a specific label, avoiding the `Vec` allocation overhead of",
            "\n[`", $method_link, "`](crate::storage::CurrentStorage::", $method_link, ").",
            "\n\n# Performance",
            "\n\n- **Zero allocation**: Holds a `MergedAdjacencyGuard` and pre-resolved label ID",
            "\n- **Lazy evaluation**: Filters as you iterate",
            "\n- **O(n) complexity**: Single linear scan regardless of match distribution",
            "\n- **Early termination**: If label doesn't exist, returns empty iterator"
        )]
        pub struct $name<'a> {
            guard: MergedAdjacencyGuard<'a>,
            frozen_index: usize,
            delta_index: usize,
            label_id: Option<InternedString>,
        }

        impl<'a> $name<'a> {
            #[doc = concat!("Create a new iterator over ", $direction, " edges with a specific label.")]
            #[inline]
            pub(crate) fn new(guard: MergedAdjacencyGuard<'a>, label_id: Option<InternedString>) -> Self {
                Self {
                    guard,
                    frozen_index: 0,
                    delta_index: 0,
                    label_id,
                }
            }
        }

        impl<'a> Iterator for $name<'a> {
            type Item = EdgeId;

            #[inline]
            fn next(&mut self) -> Option<Self::Item> {
                let label_id = self.label_id?;

                // Iterate frozen edges
                let frozen_slice = self.guard.frozen.get_adjacency(self.guard.node);
                while self.frozen_index < frozen_slice.len() {
                    let entry = &frozen_slice[self.frozen_index];
                    self.frozen_index += 1;
                    if entry.label == label_id && (self.guard.fast_path || !self.guard.tombstones.contains_key(&entry.edge_id)) {
                        return Some(entry.edge_id);
                    }
                }

                // Iterate delta edges
                if let Some(delta) = &self.guard.delta {
                    while self.delta_index < delta.len() {
                        let entry = &delta[self.delta_index];
                        self.delta_index += 1;
                        if entry.label == label_id && (self.guard.fast_path || !self.guard.tombstones.contains_key(&entry.edge_id)) {
                            return Some(entry.edge_id);
                        }
                    }
                }

                None
            }

            #[inline]
            fn size_hint(&self) -> (usize, Option<usize>) {
                (0, None)
            }
        }

        impl<'a> std::iter::FusedIterator for $name<'a> {}
    };
}

/// Macro to generate adjacency entry iterator types (Issue #405).
///
/// These iterators yield full `AdjacencyEntry` structs (containing target/source node,
/// edge ID, and label), avoiding the need for subsequent lookups to get the neighbor node.
macro_rules! impl_entry_iter {
    (
        $(#[$meta:meta])*
        $name:ident,
        $method_link:literal,
        $direction:literal
    ) => {
        $(#[$meta])*
        #[doc = concat!(
            "\n\nThis iterator provides zero-allocation traversal of ", $direction, " entries,",
            "\nyielding full `AdjacencyEntry` structs (neighbor, edge_id, label).",
            "\n\n# Performance",
            "\n\n- **Zero allocation**: Holds a `MergedAdjacencyGuard`",
            "\n- **Zero lookup**: Neighbor node ID is available directly without `DashMap` lookup",
            "\n- **Lazy evaluation**: Only computes results as you iterate",
            "\n\n# Issue #405",
            "\n\nThis iterator eliminates the need for `get_edge_target()` lookups during traversal,",
            "\nsaving one O(1) hash map lookup per edge."
        )]
        pub struct $name<'a> {
            guard: MergedAdjacencyGuard<'a>,
            frozen_index: usize,
            delta_index: usize,
        }

        impl<'a> $name<'a> {
            #[doc = concat!("Create a new iterator over ", $direction, " adjacency entries.")]
            #[inline]
            pub(crate) fn new(guard: MergedAdjacencyGuard<'a>) -> Self {
                Self { guard, frozen_index: 0, delta_index: 0 }
            }
        }

        impl<'a> Iterator for $name<'a> {
            type Item = AdjacencyEntry;

            #[inline]
            fn next(&mut self) -> Option<Self::Item> {
                // Iterate frozen edges
                let frozen_slice = self.guard.frozen.get_adjacency(self.guard.node);
                while self.frozen_index < frozen_slice.len() {
                    let entry = &frozen_slice[self.frozen_index];
                    self.frozen_index += 1;
                    if self.guard.fast_path || !self.guard.tombstones.contains_key(&entry.edge_id) {
                        return Some(*entry);
                    }
                }

                // Iterate delta edges
                if let Some(delta) = &self.guard.delta {
                    while self.delta_index < delta.len() {
                        let entry = &delta[self.delta_index];
                        self.delta_index += 1;
                        if self.guard.fast_path || !self.guard.tombstones.contains_key(&entry.edge_id) {
                            return Some(*entry);
                        }
                    }
                }

                None
            }

            #[inline]
            fn size_hint(&self) -> (usize, Option<usize>) {
                (0, None)
            }
        }

        impl<'a> ExactSizeIterator for $name<'a> {
            #[inline]
            fn len(&self) -> usize {
                // Exact size calculation is O(N) due to tombstones filtering.
                // We should probably remove ExactSizeIterator if we want to be pure lazy,
                // or just accept O(N) cost for len().
                // But ExactSizeIterator for Filter requires knowing count.
                // So we can't implement ExactSizeIterator correctly without scanning.
                // Reverting to manually counting remaining.
                let frozen_slice = self.guard.frozen.get_adjacency(self.guard.node);
                let frozen_rem = if self.guard.fast_path {
                    frozen_slice.len().saturating_sub(self.frozen_index)
                } else {
                    frozen_slice[self.frozen_index..].iter()
                        .filter(|e| !self.guard.tombstones.contains_key(&e.edge_id))
                        .count()
                };

                let delta_rem = if let Some(delta) = &self.guard.delta {
                    if self.guard.fast_path {
                        delta.len().saturating_sub(self.delta_index)
                    } else {
                        delta[self.delta_index..].iter()
                            .filter(|e| !self.guard.tombstones.contains_key(&e.edge_id))
                            .count()
                    }
                } else {
                    0
                };

                frozen_rem + delta_rem
            }
        }

        impl<'a> std::iter::FusedIterator for $name<'a> {}
    };
}

impl_edge_iter!(
    /// Iterator over outgoing edge IDs from a node.
    OutgoingEdgesIter,
    "get_outgoing_edges",
    "outgoing"
);

impl_edge_iter!(
    /// Iterator over incoming edge IDs to a node.
    IncomingEdgesIter,
    "get_incoming_edges",
    "incoming"
);

impl_edge_iter_with_label!(
    /// Iterator over outgoing edge IDs from a node, filtered by label.
    OutgoingEdgesWithLabelIter,
    "get_outgoing_edges_with_label",
    "outgoing"
);

impl_edge_iter_with_label!(
    /// Iterator over incoming edge IDs to a node, filtered by label.
    IncomingEdgesWithLabelIter,
    "get_incoming_edges_with_label",
    "incoming"
);

impl_entry_iter!(
    /// Iterator over outgoing adjacency entries (neighbor, edge_id, label).
    OutgoingEntriesIter,
    "get_outgoing_entries_iter",
    "outgoing"
);

impl_entry_iter!(
    /// Iterator over incoming adjacency entries (neighbor, edge_id, label).
    ///
    /// **Note**: For incoming edges, the `target` field of `AdjacencyEntry`
    /// represents the **source** node (neighbor).
    IncomingEntriesIter,
    "get_incoming_entries_iter",
    "incoming"
);
