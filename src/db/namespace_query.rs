//! Namespace-scoped reads and traversal (Issue #3349, PR2).
//!
//! These are the read counterpart to PR1's namespaced writes: a
//! [`NamespaceScope`] filters which entities a read may see, and — for
//! traversal — which edges it may cross. The scope is enforced against the
//! **immutable** namespace recorded on each entity (PR1's ride-along
//! property), accelerated by the secondary membership index maintained in
//! [`CurrentIndexes`](crate::index::CurrentIndexes).
//!
//! # Isolation semantics (design §4)
//!
//! - **Entity filter:** a read scoped to `S` returns only entities whose
//!   namespace ∈ `S` ([`NamespaceScope::All`] ⇒ no filter).
//! - **Traversal boundary:** from an in-scope node an edge is followed only if
//!   the **edge's own namespace ∈ S AND the target node's namespace ∈ S**. An
//!   edge whose namespace is outside `S` is never crossed, so a scope can never
//!   silently leak across the boundary. A union scope follows within and
//!   between exactly its listed namespaces.
//!
//! Omitting a scope (using [`NamespaceScope::default`], i.e.
//! `Single(default)`) reproduces exact single-agent back-compat over
//! `default`-namespace data.

use crate::core::error::{Error, Result, StorageError};
use crate::core::graph::{Edge, Node};
use crate::core::id::{EdgeId, NodeId};
use crate::core::namespace::{Namespace, NamespaceError, NamespaceScope, ResolvedScope};
use crate::core::temporal::Timestamp;
use crate::db::AletheiaDB;
use crate::db::ops::NodesAtTime;
use std::collections::{BTreeSet, HashSet, VecDeque};

impl AletheiaDB {
    /// Validate a read scope against the namespace registry (Issue #3349).
    ///
    /// # Errors
    ///
    /// - [`NamespaceError::NotFound`] (→ MCP `NOT_FOUND`, with the offending
    ///   `namespace` in the error) when the scope names a namespace that is
    ///   not registered. The implicit `default` always resolves.
    /// - An empty list scope is rejected at construction
    ///   ([`NamespaceScope::list`]); this method also rejects a `List` that
    ///   was built by other means and is empty.
    pub(crate) fn validate_scope(&self, scope: &NamespaceScope) -> Result<()> {
        // Allocation-free (Issue #3349, PR2 performance): probe the registry
        // directly per named namespace rather than materializing (and cloning)
        // the `explicit_namespaces()` vec on the scoped hot path.
        match scope {
            NamespaceScope::All => Ok(()),
            NamespaceScope::Single(ns) => self.check_namespace_registered(ns),
            NamespaceScope::List(list) => {
                if list.is_empty() {
                    return Err(Error::Namespace(NamespaceError::InvalidName {
                        name: String::new(),
                        reason: "namespace scope list must not be empty".to_string(),
                    }));
                }
                for ns in list {
                    self.check_namespace_registered(ns)?;
                }
                Ok(())
            }
        }
    }

    /// Registry membership check for a single namespace. The implicit `default`
    /// always resolves.
    #[inline]
    fn check_namespace_registered(&self, ns: &Namespace) -> Result<()> {
        if self.namespaces.get(ns.as_str()).is_none() {
            return Err(Error::Namespace(NamespaceError::NotFound {
                namespace: ns.as_str().to_string(),
            }));
        }
        Ok(())
    }

    /// O(1) membership probe: is the current node `node_id` inside the
    /// **pre-resolved** scope `resolved` (Issue #3349, PR2)?
    /// [`ResolvedScope::All`] matches everything. The scope is resolved to
    /// interned ids **once** per query (via [`NamespaceScope::resolve`]) and
    /// reused across every edge, so the per-edge boundary check is an
    /// integer-hashed membership probe with no property load, no `Namespace`
    /// allocation, and no string hashing — and, for a single-namespace scope, a
    /// single integer compare with no set scan.
    fn scope_contains_current_node(&self, node_id: NodeId, resolved: &ResolvedScope) -> bool {
        resolved.contains_by(|id| self.current.node_in_namespace_id(node_id, id))
    }

    /// O(1) membership probe for a current edge against the pre-resolved scope.
    /// See [`scope_contains_current_node`](Self::scope_contains_current_node).
    fn scope_contains_current_edge(&self, edge_id: EdgeId, resolved: &ResolvedScope) -> bool {
        resolved.contains_by(|id| self.current.edge_in_namespace_id(edge_id, id))
    }

    /// Get a node only if it is visible in `scope` (Issue #3349).
    ///
    /// Returns the node when its (immutable) namespace ∈ `scope`, otherwise
    /// [`StorageError::NodeNotFound`] — a node outside the scope is
    /// indistinguishable from a missing one, so a scoped caller can never learn
    /// that an out-of-scope node exists.
    ///
    /// # Errors
    ///
    /// - `NOT_FOUND` if the node does not exist or is not in `scope`.
    /// - `NOT_FOUND` / `INVALID_ARGUMENT` if `scope` is invalid
    ///   (see [`validate_scope`](Self::validate_scope)).
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn get_node_scoped(&self, node_id: NodeId, scope: &NamespaceScope) -> Result<Node> {
        self.validate_scope(scope)?;
        let node = self.get_node(node_id)?;
        if scope.contains(&node.namespace()) {
            Ok(node)
        } else {
            Err(StorageError::NodeNotFound(node_id).into())
        }
    }

    /// Get an edge only if it is visible in `scope` (Issue #3349).
    ///
    /// # Errors
    ///
    /// - `NOT_FOUND` if the edge does not exist or is not in `scope`.
    /// - `NOT_FOUND` / `INVALID_ARGUMENT` if `scope` is invalid.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn get_edge_scoped(&self, edge_id: EdgeId, scope: &NamespaceScope) -> Result<Edge> {
        self.validate_scope(scope)?;
        let edge = self.get_edge(edge_id)?;
        if scope.contains(&edge.namespace()) {
            Ok(edge)
        } else {
            Err(StorageError::EdgeNotFound(edge_id).into())
        }
    }

    /// List node ids visible in `scope`, optionally filtered by `label`
    /// (Issue #3349). The scoped counterpart of a `list_nodes` scan.
    ///
    /// Fast for an explicit scope: candidates come from the membership index,
    /// not a full property scan. Results are sorted by node id for
    /// deterministic pagination.
    ///
    /// # Errors
    ///
    /// `NOT_FOUND` / `INVALID_ARGUMENT` if `scope` is invalid.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn list_nodes_scoped(
        &self,
        label: Option<&str>,
        scope: &NamespaceScope,
    ) -> Result<Vec<NodeId>> {
        self.validate_scope(scope)?;
        let mut ids: Vec<NodeId> = match scope.explicit_namespaces() {
            // Explicit scope: enumerate members via the index. NodeIds are
            // globally unique and namespaces are disjoint (a node lives in
            // exactly one), and `explicit_namespaces()` already dedups the ns
            // list — so a flat extend needs no set dedup; the final
            // `sort_unstable` gives deterministic order.
            Some(namespaces) => {
                let mut ids = Vec::new();
                for ns in &namespaces {
                    ids.extend(self.current.namespace_node_ids(ns));
                }
                ids
            }
            // `All`: no namespace filter — every current node is a candidate.
            None => self.get_all_node_ids(),
        };
        if let Some(label) = label {
            let want = crate::core::interning::GLOBAL_INTERNER.intern(label)?;
            ids.retain(|id| {
                self.current
                    .get_node_label(*id)
                    .map(|l| l == want)
                    .unwrap_or(false)
            });
        }
        ids.sort_unstable();
        Ok(ids)
    }

    /// Find node ids by label + property value, filtered to `scope`
    /// (Issue #3349). The scoped counterpart of
    /// [`find_nodes_by_property`](Self::find_nodes_by_property).
    ///
    /// # Errors
    ///
    /// `NOT_FOUND` / `INVALID_ARGUMENT` if `scope` is invalid.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn find_nodes_by_property_scoped(
        &self,
        label: &str,
        property_key: &str,
        property_value: &crate::core::property::PropertyValue,
        scope: &NamespaceScope,
    ) -> Result<Vec<NodeId>> {
        self.validate_scope(scope)?;
        let mut ids = self.find_nodes_by_property(label, property_key, property_value);
        // `All` (`resolved_ids() == None`) names every namespace, so there is
        // nothing to filter. Otherwise filter via the O(1) interned-id membership
        // probe rather than cloning a whole `Node` just to read its namespace
        // (Issue #3349 B2).
        if let Some(allowed) = scope.resolved_ids() {
            ids.retain(|id| {
                allowed
                    .iter()
                    .any(|ns_id| self.current.node_in_namespace_id(*id, *ns_id))
            });
        }
        ids.sort_unstable();
        Ok(ids)
    }

    /// Breadth-first traversal from `start`, honoring the namespace scope
    /// **boundary** (Issue #3349, design §4 / test T4).
    ///
    /// Follows outgoing edges only. An edge is crossed only when BOTH the
    /// edge's own namespace ∈ `scope` AND the target node's namespace ∈
    /// `scope`; a target node outside `scope` stops traversal there. With a
    /// union scope, traversal proceeds within and between exactly the listed
    /// namespaces. `edge_label` optionally restricts which relationship type is
    /// followed.
    ///
    /// Returns the distinct set of nodes reachable within `max_depth` hops
    /// (excluding `start`), sorted by node id. `max_depth == 0` returns an
    /// empty vec.
    ///
    /// # Errors
    ///
    /// - `NOT_FOUND` if `start` does not exist or is itself outside `scope`.
    /// - `NOT_FOUND` / `INVALID_ARGUMENT` if `scope` is invalid.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn traverse_scoped(
        &self,
        start: NodeId,
        edge_label: Option<&str>,
        max_depth: usize,
        scope: &NamespaceScope,
    ) -> Result<Vec<NodeId>> {
        self.validate_scope(scope)?;
        // Resolve the scope to interned ids ONCE, then reuse across every edge —
        // the per-edge probe is then an integer hash, not a string hash, and a
        // single-namespace scope carries its one id inline for a single-compare
        // check (Issue #3349, PR2 performance hoist).
        let resolved = scope.resolve();
        // The traversal must start from an in-scope node; otherwise the caller
        // could probe out-of-scope adjacency. An out-of-scope (or non-existent)
        // start is reported as not-found, exactly like `get_node_scoped`. The
        // O(1) membership probe avoids loading the start node's properties.
        if !self.scope_contains_current_node(start, &resolved) {
            return Err(StorageError::NodeNotFound(start).into());
        }

        // A single `seen` set does double duty: it dedups the BFS frontier AND
        // is the emitted set. `start` is inserted up front so it is never
        // re-enqueued nor emitted, then removed just before returning.
        let mut seen: BTreeSet<NodeId> = BTreeSet::new();
        seen.insert(start);
        let mut queue: VecDeque<(NodeId, usize)> = VecDeque::new();
        queue.push_back((start, 0));

        while let Some((node, depth)) = queue.pop_front() {
            if depth >= max_depth {
                continue;
            }
            let edge_ids = match edge_label {
                Some(label) => self.get_outgoing_edges_with_label(node, label),
                None => self.get_outgoing_edges(node),
            };
            for edge_id in edge_ids {
                // Boundary rule via O(1) membership probes (no full edge/node
                // reconstruction): the edge's own namespace ∈ scope AND the
                // target node's namespace ∈ scope. Namespace is immutable, so the
                // current-state membership index is authoritative for the
                // (current-state) traversal.
                if !self.scope_contains_current_edge(edge_id, &resolved) {
                    continue;
                }
                let Ok(target) = self.current.get_edge_target(edge_id) else {
                    continue;
                };
                if !self.scope_contains_current_node(target, &resolved) {
                    continue;
                }
                if seen.insert(target) {
                    queue.push_back((target, depth + 1));
                }
            }
        }
        seen.remove(&start);
        Ok(seen.into_iter().collect())
    }

    /// The set of current node ids in an **explicit** scope, drawn from the
    /// membership index; `None` for [`NamespaceScope::All`] (no filter needed).
    ///
    /// Used to build a cheap `Fn(&NodeId) -> bool` scope predicate for
    /// filter-complete vector search (T6) without a per-candidate property load.
    fn scope_allowed_node_ids(&self, scope: &NamespaceScope) -> Option<HashSet<NodeId>> {
        scope.explicit_namespaces().map(|namespaces| {
            let mut allowed = HashSet::new();
            for ns in &namespaces {
                allowed.extend(self.current.namespace_node_ids(ns));
            }
            allowed
        })
    }

    /// Filter-complete k-NN from an existing node's embedding, restricted to
    /// `scope` (Issue #3349, PR2 / test T6).
    ///
    /// Returns the top-`k` most-similar nodes whose (immutable) namespace ∈
    /// `scope`. The search **over-fetches** candidates until it has `k` genuinely
    /// in-scope results (or the index is exhausted) — never "take `k`, then drop
    /// the out-of-scope ones leaving fewer than `k`". Scores and ordering of the
    /// returned in-scope results are identical to an unscoped search. The query
    /// node is excluded.
    ///
    /// The namespace filter is applied only to candidates the vector index
    /// actually returns; it makes **no assumption about why a vector might be
    /// absent** from the index (e.g. a crypto-shredded embedding is simply never
    /// a candidate). For [`NamespaceScope::All`] this is exactly the unscoped
    /// search.
    ///
    /// # Errors
    ///
    /// - `NOT_FOUND` / `INVALID_ARGUMENT` if `scope` is invalid.
    /// - The vector-search errors of the underlying index (no index enabled,
    ///   query node missing or lacking the indexed vector).
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn find_similar_scoped(
        &self,
        query_node_id: NodeId,
        k: usize,
        scope: &NamespaceScope,
    ) -> Result<Vec<(NodeId, f32)>> {
        self.validate_scope(scope)?;
        match self.scope_allowed_node_ids(scope) {
            Some(allowed) => {
                self.current
                    .find_similar_by_node_with_predicate(query_node_id, k, move |id| {
                        allowed.contains(id)
                    })
            }
            // `All`: no namespace filter (identical to the unscoped search).
            None => self
                .current
                .find_similar_by_node_with_predicate(query_node_id, k, |_| true),
        }
    }

    /// Filter-complete k-NN from a raw embedding, restricted to `scope`
    /// (Issue #3349, PR2 / test T6). The embedding counterpart of
    /// [`find_similar_scoped`](Self::find_similar_scoped) — see it for the
    /// filter-completeness and absent-vector guarantees.
    ///
    /// # Errors
    ///
    /// - `NOT_FOUND` / `INVALID_ARGUMENT` if `scope` is invalid.
    /// - The vector-search errors of the underlying index (no index enabled,
    ///   embedding dimension mismatch).
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn find_similar_by_embedding_scoped(
        &self,
        embedding: &[f32],
        k: usize,
        scope: &NamespaceScope,
    ) -> Result<Vec<(NodeId, f32)>> {
        self.validate_scope(scope)?;
        match self.scope_allowed_node_ids(scope) {
            Some(allowed) => {
                self.current
                    .find_similar_by_embedding_with_predicate(embedding, k, move |id| {
                        allowed.contains(id)
                    })
            }
            None => self
                .current
                .find_similar_by_embedding_with_predicate(embedding, k, |_| true),
        }
    }

    /// Get a node reconstructed at a bi-temporal point, visible only in `scope`
    /// (Issue #3349, PR2 / test T7).
    ///
    /// The point-in-time reconstruction runs **first**, then the namespace filter
    /// applies to the reconstructed node. Because a namespace is immutable across
    /// an entity's whole history, membership is constant — the reconstructed node
    /// carries exactly the namespace it was created in, so a past query is
    /// unaffected by later writes.
    ///
    /// # Errors
    ///
    /// - `NOT_FOUND` if the node did not exist at that point or is not in `scope`.
    /// - `NOT_FOUND` / `INVALID_ARGUMENT` if `scope` is invalid.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn get_node_at_time_scoped(
        &self,
        node_id: NodeId,
        valid_time: Timestamp,
        transaction_time: Timestamp,
        scope: &NamespaceScope,
    ) -> Result<Node> {
        self.validate_scope(scope)?;
        let node = self.get_node_at_time(node_id, valid_time, transaction_time)?;
        if scope.contains(&node.namespace()) {
            Ok(node)
        } else {
            Err(StorageError::NodeNotFound(node_id).into())
        }
    }

    /// Get an edge reconstructed at a bi-temporal point, visible only in `scope`
    /// (Issue #3349, PR2 / test T7). See
    /// [`get_node_at_time_scoped`](Self::get_node_at_time_scoped).
    ///
    /// # Errors
    ///
    /// - `NOT_FOUND` if the edge did not exist at that point or is not in `scope`.
    /// - `NOT_FOUND` / `INVALID_ARGUMENT` if `scope` is invalid.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn get_edge_at_time_scoped(
        &self,
        edge_id: EdgeId,
        valid_time: Timestamp,
        transaction_time: Timestamp,
        scope: &NamespaceScope,
    ) -> Result<Edge> {
        self.validate_scope(scope)?;
        let edge = self.get_edge_at_time(edge_id, valid_time, transaction_time)?;
        if scope.contains(&edge.namespace()) {
            Ok(edge)
        } else {
            Err(StorageError::EdgeNotFound(edge_id).into())
        }
    }

    /// Point-in-time node find by label, restricted to `scope` (Issue #3349,
    /// PR2 / test T7). The scoped counterpart of
    /// [`find_nodes_at_time`](Self::find_nodes_at_time): each candidate is
    /// reconstructed at `(valid_time, transaction_time)` first, then filtered by
    /// its (immutable) namespace.
    ///
    /// # Errors
    ///
    /// `NOT_FOUND` / `INVALID_ARGUMENT` if `scope` is invalid.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn find_nodes_at_time_scoped(
        &self,
        label: &str,
        valid_time: Timestamp,
        transaction_time: Timestamp,
        scope: &NamespaceScope,
    ) -> Result<NodesAtTime> {
        self.validate_scope(scope)?;
        let mut result = self.find_nodes_at_time(label, valid_time, transaction_time)?;
        if !matches!(scope, NamespaceScope::All) {
            result
                .nodes
                .retain(|node| scope.contains(&node.namespace()));
        }
        Ok(result)
    }

    /// Point-in-time node find by label + property, restricted to `scope`
    /// (Issue #3349, PR2 / test T7). See
    /// [`find_nodes_at_time_scoped`](Self::find_nodes_at_time_scoped).
    ///
    /// # Errors
    ///
    /// `NOT_FOUND` / `INVALID_ARGUMENT` if `scope` is invalid.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn find_nodes_by_property_at_scoped(
        &self,
        label: &str,
        property_key: &str,
        property_value: &crate::core::property::PropertyValue,
        valid_time: Timestamp,
        transaction_time: Timestamp,
        scope: &NamespaceScope,
    ) -> Result<NodesAtTime> {
        self.validate_scope(scope)?;
        let mut result = self.find_nodes_by_property_at(
            label,
            property_key,
            property_value,
            valid_time,
            transaction_time,
        )?;
        if !matches!(scope, NamespaceScope::All) {
            result
                .nodes
                .retain(|node| scope.contains(&node.namespace()));
        }
        Ok(result)
    }

    /// Breadth-first traversal from `start` **as of** a bi-temporal point,
    /// honoring the namespace scope boundary (Issue #3349, PR2 / test T7).
    ///
    /// Composes [`traverse_scoped`](Self::traverse_scoped)'s boundary semantics
    /// with point-in-time reconstruction: an edge is crossed only if it existed
    /// at `(valid_time, transaction_time)` AND its namespace ∈ `scope` AND the
    /// target node, reconstructed at that point, exists and has namespace ∈
    /// `scope`. Node/edge state (including namespace, which is immutable) reflects
    /// that coordinate, so a past query is unaffected by later writes.
    ///
    /// Candidate edges come from the current adjacency index and are then
    /// filtered by point-in-time visibility (mirroring the executor's AS OF
    /// traversal, #3225): recalling a since-deleted edge requires anchoring both
    /// dimensions before its deletion.
    ///
    /// # Errors
    ///
    /// - `NOT_FOUND` if `start` did not exist at that point or is outside `scope`.
    /// - `NOT_FOUND` / `INVALID_ARGUMENT` if `scope` is invalid.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn traverse_scoped_as_of(
        &self,
        start: NodeId,
        edge_label: Option<&str>,
        max_depth: usize,
        valid_time: Timestamp,
        transaction_time: Timestamp,
        scope: &NamespaceScope,
    ) -> Result<Vec<NodeId>> {
        self.validate_scope(scope)?;
        // The start node must exist at the coordinate AND be in scope.
        let start_node = self.get_node_at_time(start, valid_time, transaction_time)?;
        if !scope.contains(&start_node.namespace()) {
            return Err(StorageError::NodeNotFound(start).into());
        }

        let mut seen: BTreeSet<NodeId> = BTreeSet::new();
        seen.insert(start);
        let mut queue: VecDeque<(NodeId, usize)> = VecDeque::new();
        queue.push_back((start, 0));

        while let Some((node, depth)) = queue.pop_front() {
            if depth >= max_depth {
                continue;
            }
            let edge_ids = match edge_label {
                Some(label) => self.get_outgoing_edges_with_label(node, label),
                None => self.get_outgoing_edges(node),
            };
            for edge_id in edge_ids {
                // Point-in-time edge reconstruction: an edge not visible at the
                // coordinate is skipped, exactly like the AS OF executor.
                let Ok(edge) = self.get_edge_at_time(edge_id, valid_time, transaction_time) else {
                    continue;
                };
                if !scope.contains(&edge.namespace()) {
                    continue;
                }
                let target = edge.target;
                let Ok(target_node) = self.get_node_at_time(target, valid_time, transaction_time)
                else {
                    continue;
                };
                if !scope.contains(&target_node.namespace()) {
                    continue;
                }
                if seen.insert(target) {
                    queue.push_back((target, depth + 1));
                }
            }
        }
        seen.remove(&start);
        Ok(seen.into_iter().collect())
    }

    /// Per-namespace current node/edge counts (Issue #3349, design §8).
    ///
    /// Returns one entry per namespace that is either **registered** or holds
    /// at least one entity, sorted by namespace name (the implicit `default`
    /// sorts first only if its name sorts first — it is included whenever it
    /// has members or is listed). Counts are O(1) membership-index reads.
    #[must_use]
    pub fn namespace_counts(&self) -> Vec<NamespaceCount> {
        // Union of registered namespaces and namespaces that currently hold
        // entities, so an empty-but-registered namespace still appears and a
        // populated-but-unregistered one is never dropped.
        let mut names: BTreeSet<Namespace> = self.current.populated_namespaces();
        for info in self.namespaces.list() {
            // Every registered name (including the implicit `default`) is a valid
            // namespace, so `Namespace::new` always succeeds here; a malformed
            // registry entry is simply skipped rather than panicking.
            if let Ok(ns) = Namespace::new(&info.name) {
                names.insert(ns);
            }
        }
        names
            .into_iter()
            .map(|ns| NamespaceCount {
                node_count: self.current.namespace_node_count(&ns),
                edge_count: self.current.namespace_edge_count(&ns),
                name: ns.into_string(),
            })
            .collect()
    }
}

/// Per-namespace current entity counts (Issue #3349), produced by
/// [`AletheiaDB::namespace_counts`] and surfaced in
/// [`DatabaseStats`](crate::db::DatabaseStats) / schema.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub struct NamespaceCount {
    /// The namespace name.
    pub name: String,
    /// Current-state node count in this namespace (O(1) membership read).
    pub node_count: usize,
    /// Current-state edge count in this namespace (O(1) membership read).
    pub edge_count: usize,
}
