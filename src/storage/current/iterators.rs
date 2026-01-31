use crate::core::id::EdgeId;
use crate::core::interning::InternedString;
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
            index: usize,
        }

        impl<'a> $name<'a> {
            #[doc = concat!("Create a new iterator over ", $direction, " edges.")]
            #[inline]
            pub(crate) fn new(guard: MergedAdjacencyGuard<'a>) -> Self {
                Self { guard, index: 0 }
            }
        }

        impl<'a> Iterator for $name<'a> {
            type Item = EdgeId;

            #[inline]
            fn next(&mut self) -> Option<Self::Item> {
                let entry = self.guard.get(self.index)?;
                self.index += 1;
                Some(entry.edge_id)
            }

            #[inline]
            fn size_hint(&self) -> (usize, Option<usize>) {
                let remaining = self.guard.len().saturating_sub(self.index);
                (remaining, Some(remaining))
            }
        }

        impl<'a> ExactSizeIterator for $name<'a> {
            #[inline]
            fn len(&self) -> usize {
                self.guard.len().saturating_sub(self.index)
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
            index: usize,
            label_id: Option<InternedString>,
        }

        impl<'a> $name<'a> {
            #[doc = concat!("Create a new iterator over ", $direction, " edges with a specific label.")]
            #[inline]
            pub(crate) fn new(guard: MergedAdjacencyGuard<'a>, label_id: Option<InternedString>) -> Self {
                Self {
                    guard,
                    index: 0,
                    label_id,
                }
            }
        }

        impl<'a> Iterator for $name<'a> {
            type Item = EdgeId;

            #[inline]
            fn next(&mut self) -> Option<Self::Item> {
                // Early return if label doesn't exist in interner
                let label_id = self.label_id?;

                // Linear scan with manual indexing - O(n) total complexity
                // Avoids the O(n²) worst-case of using .position() repeatedly
                while self.index < self.guard.len() {
                    let entry = &self.guard[self.index];
                    self.index += 1;
                    if entry.label == label_id {
                        return Some(entry.edge_id);
                    }
                }
                None
            }

            #[inline]
            fn size_hint(&self) -> (usize, Option<usize>) {
                // Lower bound is 0 (all remaining could be filtered out)
                // Upper bound is remaining entries
                let remaining = self.guard.len().saturating_sub(self.index);
                (0, Some(remaining))
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
