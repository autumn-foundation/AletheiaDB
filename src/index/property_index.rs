//! Secondary equality index for `find_nodes_by_property` (opt-in, node-only).
//!
//! # What this is
//!
//! An **opt-in, node-only, current-state, rebuilt-on-load** secondary index that
//! replaces the O(nodes-per-label) equality scan in
//! [`CurrentStorage::find_nodes_by_property`](crate::storage::current::CurrentStorage::find_nodes_by_property)
//! with an O(matches) map probe. It mirrors the namespace membership index
//! (Issue #3349) and the vector index (Issue #389): a derived DashMap living on
//! [`CurrentIndexes`](crate::index::current::CurrentIndexes), maintained inside
//! the `insert_node` / `remove_node` choke points so every write path — create,
//! update, delete, cascade, retract, WAL replay, index-persistence load — keeps
//! it consistent for free.
//!
//! # Deliberate scope boundaries
//!
//! - **Current-state only.** Point-in-time / historical reads
//!   (`find_nodes_by_property_at`) keep using the historical scan; this index is
//!   never consulted for a temporal query.
//! - **Node-only.** There is no `find_edges_by_property` today; an edge index is
//!   a clean follow-up via `insert_edge` / `remove_edge`.
//! - **Indexable value types are `String`, `Int`, `Bool` only.** A lookup whose
//!   value is any other variant — notably [`PropertyValue::Float`] — transparently
//!   falls back to the existing scan and returns byte-identical results.
//! - **Rebuilt-on-load, never persisted.** The index is a pure function of
//!   current node state and is reconstructed from the live interner at load, so it
//!   can never diverge from the WAL / `.albk` and needs no format change. This
//!   also sidesteps the persisted-interner-id remap hazard (Issues #3490/#3506).
//!
//! # Why `Float` is excluded (correctness, not laziness)
//!
//! [`PropertyValue`]'s equality compares `Float` **bitwise** (`a == b` on `f64`):
//! `NaN != NaN` and `-0.0 == 0.0`. A hash index keyed on the raw bits would make
//! `NaN` unfindable and split `-0.0`/`0.0` into separate buckets, so the index
//! would *disagree* with the scan's `==`. Rather than risk a silent
//! index-vs-scan divergence, `Float` (and `Bytes`/`Array`/`Vector`/`SparseVector`)
//! fall back to the scan. A canonicalizing float index with an explicit NaN
//! policy is a deferred follow-up.
//!
//! # Namespace fail-closed
//!
//! The index key intentionally does **not** fold in the namespace. Scoped reads
//! stay fail-closed by reusing the existing O(1) `node_in_namespace_id` retain
//! over the (now tiny) index hit set — exactly the post-filter the scan-based
//! scoped path already ran, but over O(matches) instead of O(N).
//!
//! # Lock discipline
//!
//! Like `ns_nodes`, the backing maps are a **leaf** in the lock-acquisition order:
//! lock-free `DashMap`s mutated only inside `insert_node` / `remove_node`, never
//! calling back into `historical`, `wal`, or `current_timestamp`.

use crate::core::property::PropertyValue;

/// A hashable, totally-comparable projection of the indexable subset of
/// [`PropertyValue`] used as the value component of a property-index key.
///
/// Only `String`, `Int`, and `Bool` are representable; every other variant maps
/// to `None` via [`value_key`] and forces a scan fallback. `Str` owns its bytes
/// (a `Box<str>`) so two distinct `Arc<str>` "Alice"s key identically — the key
/// is by value, never by `Arc` pointer identity.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ValueKey {
    /// A boolean property value.
    Bool(bool),
    /// A 64-bit signed integer property value.
    Int(i64),
    /// A UTF-8 string property value (owned, compared by bytes).
    Str(Box<str>),
}

/// Project a [`PropertyValue`] onto a [`ValueKey`], or `None` when the variant is
/// not indexable (and the caller must fall back to the equality scan).
///
/// Indexable: `Bool`, `Int`, `String`. Non-indexable (returns `None`): `Null`,
/// `Float` (bitwise/NaN hazard — see the module docs), `Bytes`, `Array`,
/// `Vector`, `SparseVector`.
#[inline]
#[must_use]
pub fn value_key(value: &PropertyValue) -> Option<ValueKey> {
    match value {
        PropertyValue::Bool(b) => Some(ValueKey::Bool(*b)),
        PropertyValue::Int(i) => Some(ValueKey::Int(*i)),
        PropertyValue::String(s) => Some(ValueKey::Str(Box::from(&**s))),
        // Deliberately not indexable — see module docs.
        PropertyValue::Null
        | PropertyValue::Float(_)
        | PropertyValue::Bytes(_)
        | PropertyValue::Array(_)
        | PropertyValue::Vector(_)
        | PropertyValue::SparseVector(_) => None,
    }
}

/// Public descriptor of one enabled property index, returned by
/// [`AletheiaDB::list_property_indexes`](crate::db::AletheiaDB::list_property_indexes).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PropertyIndexInfo {
    /// The node label the index is scoped to.
    pub label: String,
    /// The property key the index covers.
    pub property: String,
}
