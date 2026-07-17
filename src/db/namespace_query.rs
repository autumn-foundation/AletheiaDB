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
use crate::core::namespace::{Namespace, NamespaceError, NamespaceScope};
use crate::db::AletheiaDB;
use std::collections::{BTreeSet, VecDeque};

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
        if let NamespaceScope::List(list) = scope
            && list.is_empty()
        {
            return Err(Error::Namespace(NamespaceError::InvalidName {
                name: String::new(),
                reason: "namespace scope list must not be empty".to_string(),
            }));
        }
        let Some(namespaces) = scope.explicit_namespaces() else {
            return Ok(()); // `All` names no finite set — nothing to check.
        };
        for ns in namespaces {
            if self.namespaces.get(ns.as_str()).is_none() {
                return Err(Error::Namespace(NamespaceError::NotFound {
                    namespace: ns.into_string(),
                }));
            }
        }
        Ok(())
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
            // Explicit scope: enumerate members via the index.
            Some(namespaces) => {
                let mut set = BTreeSet::new();
                for ns in &namespaces {
                    set.extend(self.current.namespace_node_ids(ns));
                }
                set.into_iter().collect()
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
        ids.retain(|id| match self.get_node(*id) {
            Ok(node) => scope.contains(&node.namespace()),
            Err(_) => false,
        });
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
        // The traversal must start from an in-scope node; otherwise the caller
        // could probe out-of-scope adjacency. An out-of-scope start is
        // reported as not-found, exactly like `get_node_scoped`.
        let start_node = self.get_node(start)?;
        if !scope.contains(&start_node.namespace()) {
            return Err(StorageError::NodeNotFound(start).into());
        }

        let mut visited: BTreeSet<NodeId> = BTreeSet::new();
        let mut queue: VecDeque<(NodeId, usize)> = VecDeque::new();
        queue.push_back((start, 0));
        let mut seen: BTreeSet<NodeId> = BTreeSet::new();
        seen.insert(start);

        while let Some((node, depth)) = queue.pop_front() {
            if depth >= max_depth {
                continue;
            }
            let edge_ids = match edge_label {
                Some(label) => self.get_outgoing_edges_with_label(node, label),
                None => self.get_outgoing_edges(node),
            };
            for edge_id in edge_ids {
                let Ok(edge) = self.get_edge(edge_id) else {
                    continue;
                };
                // Boundary rule: the edge's own namespace must be in scope.
                if !scope.contains(&edge.namespace()) {
                    continue;
                }
                let target = edge.target;
                // ...and the target node must exist AND be in scope.
                let Ok(target_node) = self.get_node(target) else {
                    continue;
                };
                if !scope.contains(&target_node.namespace()) {
                    continue;
                }
                if seen.insert(target) {
                    queue.push_back((target, depth + 1));
                }
                visited.insert(target);
            }
        }
        Ok(visited.into_iter().collect())
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
            if let Ok(ns) = Namespace::new(&info.name) {
                names.insert(ns);
            } else if info.name == Namespace::DEFAULT {
                names.insert(Namespace::default());
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
