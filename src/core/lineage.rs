//! Derivation lineage between facts (Issue #3371).
//!
//! Write-time *attributive* provenance ([`crate::core::provenance`]) answers
//! "where did this fact come from?" for a fact written directly from an
//! external source. It cannot answer the question that dominates real
//! knowledge pipelines: **"which facts was this fact computed from?"** When an
//! ETL job merges three records into one, when an agent summarizes ten
//! documents into a claim, or when entity resolution collapses duplicates, the
//! output fact's true provenance is *other facts in this database*, not a
//! source string.
//!
//! This module records that derivation structure at write time and makes it
//! queryable as a first-class, bi-temporally precise dimension. A
//! [`LineageRecord`] declares that a *derived* fact version was computed from a
//! set of *source* fact versions. Because every reference pins a specific
//! [`VersionId`] (not just an entity id), lineage refers to exactly the fact
//! that was read — later updates to an input never silently rewrite what a
//! derivation was based on (the failure mode of modelling lineage as ordinary
//! graph edges pointing at *current* nodes).
//!
//! # Version-space is a DAG
//!
//! A declared source must already exist when the derived version is written,
//! so its [`VersionId`] was always allocated by an *earlier* write than the
//! derived version's. Lineage edges therefore always point from a
//! higher-numbered version to lower-numbered versions, which makes the lineage
//! graph acyclic by construction. The cycle and self-derivation guards in
//! [`LineageStore::record`] are defence-in-depth (and directly unit-tested);
//! they cannot fire through the normal create/update write path.
//!
//! # Durability (v1 limitation)
//!
//! To avoid changing the WAL entry format (which is being restructured under
//! Issue #3413), the v1 [`LineageStore`] is an in-memory index. Lineage
//! records are immutable once written and survive supersession/retraction of
//! the facts they reference, but they do **not** yet survive a process
//! restart. Rehydrating lineage from a persisted log is a tracked follow-up;
//! the record shape is deliberately serialisation-friendly for that work.

use std::collections::{HashSet, VecDeque};

use dashmap::DashMap;

use crate::core::id::{EntityId, VersionId};
use crate::core::temporal::Timestamp;

/// A version-pinned reference to a single fact version — the unit of lineage.
///
/// Pins both *which* entity ([`EntityId`], a node or an edge) and *which
/// version* of it ([`VersionId`]) was read, so lineage stays precise across
/// later updates to that entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LineageRef {
    /// The entity (node or edge) the referenced version belongs to.
    pub entity: EntityId,
    /// The specific version that was read/derived.
    pub version: VersionId,
}

impl LineageRef {
    /// Construct a reference to a specific version of an entity.
    #[inline]
    pub fn new(entity: impl Into<EntityId>, version: VersionId) -> Self {
        Self {
            entity: entity.into(),
            version,
        }
    }
}

/// An immutable record declaring that one fact version was derived from a set
/// of source fact versions, recorded at a transaction time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineageRecord {
    /// The fact version that was produced.
    pub derived: LineageRef,
    /// The fact versions it was computed from (order preserved as declared;
    /// deduplicated).
    pub sources: Vec<LineageRef>,
    /// The transaction time at which this derivation was recorded. Used to
    /// answer `AS OF` lineage queries: a record is invisible to a query whose
    /// `as_of` transaction time precedes `recorded_at`.
    pub recorded_at: Timestamp,
}

/// Errors that can occur when recording a [`LineageRecord`].
///
/// Source-existence validation lives at the database layer (which owns the
/// historical store); the errors here are the structural guards the store
/// itself enforces.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LineageError {
    /// A declared source reference does not resolve to an existing fact
    /// version (unknown version, or a version whose entity does not match the
    /// reference). The write is rejected before any commit — no silent
    /// dangling lineage.
    #[error(
        "derivation source {entity} @ version {version} does not exist",
        entity = .reference.entity,
        version = .reference.version.as_u64()
    )]
    SourceNotFound {
        /// The unresolved source reference.
        reference: LineageRef,
    },

    /// A version was declared as derived from itself.
    #[error("fact {0} cannot be derived from itself")]
    SelfDerivation(VersionId),

    /// Recording the derivation would close a cycle: the derived version is
    /// already reachable by walking upstream from one of the declared
    /// sources. The path names the cycle, from the offending source up to the
    /// derived version.
    #[error("derivation would create a cycle: {}", format_cycle(.path))]
    CycleDetected {
        /// The upstream path from a declared source to the derived version
        /// that closes the cycle (source first, derived version last).
        path: Vec<VersionId>,
    },

    /// A derivation was already recorded for this derived version. Lineage is
    /// write-once per version (versions are immutable); re-declaring is
    /// rejected rather than silently overwriting recorded history.
    #[error("lineage already recorded for fact {0}")]
    AlreadyRecorded(VersionId),
}

fn format_cycle(path: &[VersionId]) -> String {
    path.iter()
        .map(|v| v.as_u64().to_string())
        .collect::<Vec<_>>()
        .join(" -> ")
}

/// Options bounding a lineage closure query.
#[derive(Debug, Clone, Copy)]
pub struct LineageQueryOptions {
    /// Maximum transitive depth to expand (hops from the root). Depth `0`
    /// returns an empty closure; depth `1` returns only direct
    /// parents/children.
    pub max_depth: usize,
    /// Maximum number of entries to return. When the traversal would exceed
    /// this, it stops and the closure's `has_more` flag is set.
    pub limit: usize,
    /// Optional `AS OF` transaction-time bound: only lineage records with
    /// `recorded_at <= as_of` are followed, so the closure reflects lineage as
    /// it was recorded by that transaction time. `None` uses all records.
    pub as_of: Option<Timestamp>,
}

impl LineageQueryOptions {
    /// Default depth cap used when a caller does not specify one.
    pub const DEFAULT_MAX_DEPTH: usize = 32;
    /// Default entry cap used when a caller does not specify one.
    pub const DEFAULT_LIMIT: usize = 1000;

    /// Options with the default depth and limit and no `AS OF` bound.
    pub fn new() -> Self {
        Self {
            max_depth: Self::DEFAULT_MAX_DEPTH,
            limit: Self::DEFAULT_LIMIT,
            as_of: None,
        }
    }

    /// Set the maximum transitive depth.
    #[must_use]
    pub fn with_max_depth(mut self, max_depth: usize) -> Self {
        self.max_depth = max_depth;
        self
    }

    /// Set the maximum number of returned entries.
    #[must_use]
    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    /// Set the `AS OF` transaction-time bound.
    #[must_use]
    pub fn with_as_of(mut self, as_of: Timestamp) -> Self {
        self.as_of = Some(as_of);
        self
    }
}

impl Default for LineageQueryOptions {
    fn default() -> Self {
        Self::new()
    }
}

/// One node in a lineage closure: a referenced fact version and the minimum
/// number of hops at which it was reached from the query root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineageEntry {
    /// The reached fact version.
    pub reference: LineageRef,
    /// Minimum hop distance from the query root (`1` = direct
    /// parent/child).
    pub depth: usize,
}

/// The result of an upstream or downstream lineage closure query.
///
/// Entries are ordered by increasing depth (breadth-first), then by the order
/// they were discovered, and never include the query root itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineageClosure {
    /// The fact the closure was computed from.
    pub root: LineageRef,
    /// The reachable fact versions with their minimum depth.
    pub entries: Vec<LineageEntry>,
    /// `true` if traversal stopped because it hit the entry `limit` or the
    /// `max_depth` bound while more of the graph remained unexplored — mirrors
    /// the `has_more` pagination convention (Issue #3226).
    pub has_more: bool,
}

impl LineageClosure {
    /// Number of distinct fact versions in the closure (excludes the root).
    #[inline]
    pub fn count(&self) -> usize {
        self.entries.len()
    }

    /// The greatest depth present in the closure (`0` for an empty closure).
    pub fn max_depth(&self) -> usize {
        self.entries.iter().map(|e| e.depth).max().unwrap_or(0)
    }
}

/// An immutable, in-memory index of fact-to-fact derivation lineage.
///
/// Maintains two adjacency maps keyed by the globally-unique [`VersionId`]:
/// `upstream` (a derived version -> the record naming its sources) and
/// `downstream` (a source version -> the versions derived from it). Closures
/// in either direction are breadth-first walks over these maps.
#[derive(Debug, Default)]
pub struct LineageStore {
    /// derived version -> its lineage record (one per version; write-once).
    upstream: DashMap<VersionId, LineageRecord>,
    /// source version -> derived versions that declared it as a source.
    downstream: DashMap<VersionId, Vec<VersionId>>,
    /// Serializes the check-then-act critical section of [`Self::record`] so
    /// the `contains_key` write-once check, the cycle scan, and the paired
    /// `upstream`/`downstream` inserts are one atomic step. Without it, two
    /// concurrent records of the *same* derived version could both pass the
    /// `contains_key` check and the second would silently overwrite the first
    /// (a lost update violating write-once). Reads (closure queries) never
    /// take this lock, so they stay fully concurrent — only the low-frequency
    /// write path is serialized.
    write_lock: parking_lot::Mutex<()>,
}

impl LineageStore {
    /// Create an empty lineage store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of derivation records held.
    pub fn len(&self) -> usize {
        self.upstream.len()
    }

    /// Whether any derivation has been recorded.
    pub fn is_empty(&self) -> bool {
        self.upstream.is_empty()
    }

    /// Record that `derived` was computed from `sources` at `recorded_at`.
    ///
    /// Sources are deduplicated preserving first-seen order. An empty (or
    /// entirely self-referential) source set is a no-op that records nothing
    /// and returns `Ok(())` — callers normalise "no lineage" to not calling
    /// this at all, but an all-duplicates-removed empty set must not fabricate
    /// a record.
    ///
    /// # Errors
    ///
    /// - [`LineageError::SelfDerivation`] if a source names the derived
    ///   version.
    /// - [`LineageError::CycleDetected`] if the derived version is already
    ///   reachable upstream from a source (structurally impossible via the
    ///   normal write path; see the module docs).
    /// - [`LineageError::AlreadyRecorded`] if a derivation was already
    ///   recorded for `derived.version` (lineage is write-once per version).
    pub fn record(
        &self,
        derived: LineageRef,
        sources: Vec<LineageRef>,
        recorded_at: Timestamp,
    ) -> Result<(), LineageError> {
        // Deduplicate sources, preserving declaration order, and reject
        // self-derivation. This is pure (touches no shared state) so it runs
        // outside the write lock.
        let mut seen = HashSet::new();
        let mut deduped: Vec<LineageRef> = Vec::with_capacity(sources.len());
        for source in sources {
            if source.version == derived.version {
                return Err(LineageError::SelfDerivation(derived.version));
            }
            if seen.insert(source.version) {
                deduped.push(source);
            }
        }

        if deduped.is_empty() {
            return Ok(());
        }

        // Everything below mutates or reads-for-mutation the shared adjacency
        // maps and must be a single atomic step: two concurrent records of the
        // same derived version must not both pass the write-once check, and the
        // cycle scan must see a consistent graph. Hold the write lock across the
        // whole check-then-act region. (Reads never take this lock.)
        let _guard = self.write_lock.lock();

        if self.upstream.contains_key(&derived.version) {
            return Err(LineageError::AlreadyRecorded(derived.version));
        }

        // Cycle guard: for each source, ensure the derived version is not
        // already reachable by walking upstream from that source.
        for source in &deduped {
            if let Some(path) = self.upstream_path_to(source.version, derived.version) {
                return Err(LineageError::CycleDetected { path });
            }
        }

        for source in &deduped {
            self.downstream
                .entry(source.version)
                .or_default()
                .push(derived.version);
        }
        self.upstream.insert(
            derived.version,
            LineageRecord {
                derived,
                sources: deduped,
                recorded_at,
            },
        );
        Ok(())
    }

    /// The recorded derivation for `version`, if any.
    pub fn record_for(&self, version: VersionId) -> Option<LineageRecord> {
        self.upstream.get(&version).map(|r| r.value().clone())
    }

    /// Direct sources (parents) of `version`.
    pub fn direct_upstream(&self, version: VersionId) -> Vec<LineageRef> {
        self.upstream
            .get(&version)
            .map(|r| r.value().sources.clone())
            .unwrap_or_default()
    }

    /// If `derived` is reachable by walking upstream from `start`, return the
    /// path `[start, ..., derived]`; otherwise `None`. Used for cycle
    /// detection.
    fn upstream_path_to(&self, start: VersionId, target: VersionId) -> Option<Vec<VersionId>> {
        // BFS with parent tracking so we can reconstruct the path.
        let mut visited = HashSet::new();
        let mut parents: std::collections::HashMap<VersionId, VersionId> =
            std::collections::HashMap::new();
        let mut queue = VecDeque::new();
        visited.insert(start);
        queue.push_back(start);
        while let Some(current) = queue.pop_front() {
            if current == target {
                // Reconstruct path from start to target.
                let mut path = vec![target];
                let mut node = target;
                while let Some(&parent) = parents.get(&node) {
                    path.push(parent);
                    node = parent;
                }
                path.reverse();
                return Some(path);
            }
            if let Some(record) = self.upstream.get(&current) {
                for source in &record.value().sources {
                    if visited.insert(source.version) {
                        parents.insert(source.version, current);
                        queue.push_back(source.version);
                    }
                }
            }
        }
        None
    }

    /// Compute the transitive upstream closure ("what was this derived
    /// from?") of `root`, bounded by `options`.
    pub fn upstream_closure(
        &self,
        root: LineageRef,
        options: LineageQueryOptions,
    ) -> LineageClosure {
        self.closure(root, options, Direction::Upstream)
    }

    /// Compute the transitive downstream closure ("what has been derived from
    /// this?") of `root`, bounded by `options` — the retraction blast-radius
    /// report.
    pub fn downstream_closure(
        &self,
        root: LineageRef,
        options: LineageQueryOptions,
    ) -> LineageClosure {
        self.closure(root, options, Direction::Downstream)
    }

    fn closure(
        &self,
        root: LineageRef,
        options: LineageQueryOptions,
        direction: Direction,
    ) -> LineageClosure {
        let mut entries = Vec::new();
        let mut visited = HashSet::new();
        visited.insert(root.version);
        let mut has_more = false;

        // BFS frontier of (reference, depth).
        let mut frontier: Vec<LineageRef> = vec![root];
        let mut depth = 0usize;

        'outer: while !frontier.is_empty() && depth < options.max_depth {
            depth += 1;
            let mut next: Vec<LineageRef> = Vec::new();
            for node in &frontier {
                let neighbours = match direction {
                    Direction::Upstream => self.neighbours_upstream(node.version, options.as_of),
                    Direction::Downstream => {
                        self.neighbours_downstream(node.version, options.as_of)
                    }
                };
                for neighbour in neighbours {
                    if !visited.insert(neighbour.version) {
                        continue;
                    }
                    if entries.len() >= options.limit {
                        has_more = true;
                        break 'outer;
                    }
                    entries.push(LineageEntry {
                        reference: neighbour,
                        depth,
                    });
                    next.push(neighbour);
                }
            }
            // If we stopped expanding because of the depth cap, flag has_more
            // only when the unexplored frontier actually has an unvisited
            // neighbour beyond the cap — a node whose one more hop would reveal
            // a new entry. A frontier of pure leaves (nothing further upstream/
            // downstream) means the closure is complete, so has_more stays
            // false (returning exactly the chain at its own length is not
            // "truncated").
            if depth == options.max_depth
                && self.frontier_has_unvisited_neighbour(&next, direction, options.as_of, &visited)
            {
                has_more = true;
            }
            frontier = next;
        }

        LineageClosure {
            root,
            entries,
            has_more,
        }
    }

    /// Whether any node in the (depth-capped) frontier has at least one
    /// neighbour, in `direction`, that has not already been visited — i.e.
    /// expanding one more hop would yield a genuinely new closure entry. Used
    /// to decide `has_more` precisely at the depth cap so a fully-returned
    /// chain is not falsely marked truncated.
    fn frontier_has_unvisited_neighbour(
        &self,
        frontier: &[LineageRef],
        direction: Direction,
        as_of: Option<Timestamp>,
        visited: &HashSet<VersionId>,
    ) -> bool {
        frontier.iter().any(|node| {
            let neighbours = match direction {
                Direction::Upstream => self.neighbours_upstream(node.version, as_of),
                Direction::Downstream => self.neighbours_downstream(node.version, as_of),
            };
            neighbours.iter().any(|nb| !visited.contains(&nb.version))
        })
    }

    /// Upstream neighbours of `version` (its declared sources), filtered by the
    /// optional `as_of` transaction-time bound.
    fn neighbours_upstream(&self, version: VersionId, as_of: Option<Timestamp>) -> Vec<LineageRef> {
        match self.upstream.get(&version) {
            Some(record) => {
                let record = record.value();
                if let Some(as_of) = as_of
                    && record.recorded_at > as_of
                {
                    return Vec::new();
                }
                record.sources.clone()
            }
            None => Vec::new(),
        }
    }

    /// Downstream neighbours of `version` (versions derived from it), filtered
    /// by the optional `as_of` transaction-time bound.
    fn neighbours_downstream(
        &self,
        version: VersionId,
        as_of: Option<Timestamp>,
    ) -> Vec<LineageRef> {
        let derived_versions = match self.downstream.get(&version) {
            Some(list) => list.value().clone(),
            None => return Vec::new(),
        };
        let mut out = Vec::with_capacity(derived_versions.len());
        for dv in derived_versions {
            if let Some(record) = self.upstream.get(&dv) {
                let record = record.value();
                if let Some(as_of) = as_of
                    && record.recorded_at > as_of
                {
                    continue;
                }
                out.push(record.derived);
            }
        }
        out
    }
}

#[derive(Debug, Clone, Copy)]
enum Direction {
    Upstream,
    Downstream,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::id::{EdgeId, NodeId};

    fn node_ref(node: u64, version: u64) -> LineageRef {
        LineageRef::new(
            NodeId::new(node).expect("valid node id"),
            VersionId::new(version).expect("valid version id"),
        )
    }

    fn ts(micros: i64) -> Timestamp {
        Timestamp::new_unchecked(micros, 0)
    }

    #[test]
    fn record_and_read_direct_upstream() {
        let store = LineageStore::new();
        let derived = node_ref(1, 100);
        let s1 = node_ref(2, 10);
        let s2 = node_ref(3, 11);
        store
            .record(derived, vec![s1, s2], ts(1000))
            .expect("record ok");

        let sources = store.direct_upstream(derived.version);
        assert_eq!(sources, vec![s1, s2]);
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn empty_sources_records_nothing() {
        let store = LineageStore::new();
        let derived = node_ref(1, 100);
        store.record(derived, vec![], ts(1)).expect("no-op ok");
        assert!(store.is_empty());
    }

    #[test]
    fn self_derivation_rejected() {
        let store = LineageStore::new();
        let derived = node_ref(1, 100);
        let err = store
            .record(derived, vec![node_ref(1, 100)], ts(1))
            .expect_err("must reject");
        assert_eq!(err, LineageError::SelfDerivation(derived.version));
    }

    #[test]
    fn duplicate_sources_deduplicated() {
        let store = LineageStore::new();
        let derived = node_ref(1, 100);
        let s1 = node_ref(2, 10);
        store
            .record(derived, vec![s1, s1, s1], ts(1))
            .expect("record ok");
        assert_eq!(store.direct_upstream(derived.version), vec![s1]);
    }

    #[test]
    fn already_recorded_rejected() {
        let store = LineageStore::new();
        let derived = node_ref(1, 100);
        store
            .record(derived, vec![node_ref(2, 10)], ts(1))
            .expect("first ok");
        let err = store
            .record(derived, vec![node_ref(3, 11)], ts(2))
            .expect_err("second rejected");
        assert_eq!(err, LineageError::AlreadyRecorded(derived.version));
    }

    #[test]
    fn transitive_cycle_rejected_at_store_level() {
        // A(v30) <- B(v20) <- C(v10), then declare C(v10) derived from A(v30):
        // walking upstream from A reaches C -> cycle.
        let store = LineageStore::new();
        let a = node_ref(1, 30);
        let b = node_ref(2, 20);
        let c = node_ref(3, 10);
        store.record(a, vec![b], ts(1)).expect("a<-b");
        store.record(b, vec![c], ts(2)).expect("b<-c");
        let err = store.record(c, vec![a], ts(3)).expect_err("cycle");
        match err {
            LineageError::CycleDetected { path } => {
                assert_eq!(path.first(), Some(&a.version));
                assert_eq!(path.last(), Some(&c.version));
            }
            other => panic!("expected cycle, got {other:?}"),
        }
    }

    #[test]
    fn upstream_closure_transitive_with_depth() {
        // D(v300) <- C(v200) <- B(v100) <- A(v10)
        let store = LineageStore::new();
        let d = node_ref(4, 300);
        let c = node_ref(3, 200);
        let b = node_ref(2, 100);
        let a = node_ref(1, 10);
        store.record(d, vec![c], ts(1)).unwrap();
        store.record(c, vec![b], ts(1)).unwrap();
        store.record(b, vec![a], ts(1)).unwrap();

        // Full closure.
        let closure = store.upstream_closure(d, LineageQueryOptions::new());
        let versions: Vec<u64> = closure
            .entries
            .iter()
            .map(|e| e.reference.version.as_u64())
            .collect();
        assert_eq!(versions, vec![200, 100, 10]);
        assert_eq!(closure.entries[0].depth, 1);
        assert_eq!(closure.entries[2].depth, 3);
        assert!(!closure.has_more);

        // Depth-limited to 1 hop: only C, has_more set.
        let shallow = store.upstream_closure(d, LineageQueryOptions::new().with_max_depth(1));
        assert_eq!(shallow.count(), 1);
        assert_eq!(shallow.entries[0].reference, c);
        assert!(shallow.has_more);
    }

    #[test]
    fn downstream_closure_blast_radius() {
        // A(v10) is the poisoned input. B and C derived from A; D derived from B.
        let store = LineageStore::new();
        let a = node_ref(1, 10);
        let b = node_ref(2, 20);
        let c = node_ref(3, 30);
        let d = node_ref(4, 40);
        store.record(b, vec![a], ts(1)).unwrap();
        store.record(c, vec![a], ts(1)).unwrap();
        store.record(d, vec![b], ts(1)).unwrap();

        let closure = store.downstream_closure(a, LineageQueryOptions::new());
        assert_eq!(closure.count(), 3);
        let mut versions: Vec<u64> = closure
            .entries
            .iter()
            .map(|e| e.reference.version.as_u64())
            .collect();
        versions.sort_unstable();
        assert_eq!(versions, vec![20, 30, 40]);
        // B and C at depth 1, D at depth 2.
        let d_entry = closure.entries.iter().find(|e| e.reference == d).unwrap();
        assert_eq!(d_entry.depth, 2);
        assert_eq!(closure.max_depth(), 2);
    }

    #[test]
    fn downstream_limit_sets_has_more() {
        let store = LineageStore::new();
        let a = node_ref(1, 10);
        for (i, v) in (20..25).enumerate() {
            store
                .record(node_ref(2 + i as u64, v), vec![a], ts(1))
                .unwrap();
        }
        let closure = store.downstream_closure(a, LineageQueryOptions::new().with_limit(2));
        assert_eq!(closure.count(), 2);
        assert!(closure.has_more);
    }

    #[test]
    fn as_of_hides_later_recorded_lineage() {
        let store = LineageStore::new();
        let b = node_ref(2, 20);
        let a = node_ref(1, 10);
        // Lineage recorded at tx time 5000.
        store.record(b, vec![a], ts(5000)).unwrap();

        // AS OF before the record: closure is empty.
        let before = store.upstream_closure(b, LineageQueryOptions::new().with_as_of(ts(4000)));
        assert_eq!(before.count(), 0);
        // AS OF at/after the record: closure resolves.
        let after = store.upstream_closure(b, LineageQueryOptions::new().with_as_of(ts(5000)));
        assert_eq!(after.count(), 1);
        assert_eq!(after.entries[0].reference, a);
    }

    #[test]
    fn version_pinning_survives_input_supersession() {
        // B(v20) derived from A's OLD version v10. A is later updated to v11.
        // The pinned upstream ref must remain v10, not v11.
        let store = LineageStore::new();
        let a_old = node_ref(1, 10);
        let b = node_ref(2, 20);
        store.record(b, vec![a_old], ts(1)).unwrap();

        let closure = store.upstream_closure(b, LineageQueryOptions::new());
        assert_eq!(
            closure.entries[0].reference.version,
            VersionId::new(10).unwrap()
        );
    }

    #[test]
    fn edge_derived_from_nodes() {
        // An edge fact can be derived from node facts (cross-kind lineage).
        let store = LineageStore::new();
        let edge = LineageRef::new(EdgeId::new(7).unwrap(), VersionId::new(500).unwrap());
        let n1 = node_ref(1, 10);
        let n2 = node_ref(2, 11);
        store.record(edge, vec![n1, n2], ts(1)).unwrap();
        assert_eq!(store.direct_upstream(edge.version), vec![n1, n2]);
        // Downstream of a node reaches the edge.
        let closure = store.downstream_closure(n1, LineageQueryOptions::new());
        assert_eq!(closure.count(), 1);
        assert_eq!(closure.entries[0].reference, edge);
    }

    #[test]
    fn explicit_two_cycle_rejected() {
        // record(a <- b) then record(b <- a): walking upstream from a reaches b
        // whose only source is a's derived version -> the second record closes
        // a 2-cycle and must be rejected.
        let store = LineageStore::new();
        let a = node_ref(1, 20);
        let b = node_ref(2, 10);
        store.record(a, vec![b], ts(1)).expect("a<-b");
        let err = store.record(b, vec![a], ts(2)).expect_err("2-cycle");
        match err {
            LineageError::CycleDetected { path } => {
                assert_eq!(path.first(), Some(&a.version));
                assert_eq!(path.last(), Some(&b.version));
            }
            other => panic!("expected cycle, got {other:?}"),
        }
    }

    #[test]
    fn depth_boundary_exact_vs_one_past() {
        // Chain D(v40) <- C(v30) <- B(v20) <- A(v10): 3 hops total.
        let store = LineageStore::new();
        let d = node_ref(4, 40);
        let c = node_ref(3, 30);
        let b = node_ref(2, 20);
        let a = node_ref(1, 10);
        store.record(d, vec![c], ts(1)).unwrap();
        store.record(c, vec![b], ts(1)).unwrap();
        store.record(b, vec![a], ts(1)).unwrap();

        // Exactly at the chain length: full closure, nothing beyond -> no has_more.
        let exact = store.upstream_closure(d, LineageQueryOptions::new().with_max_depth(3));
        assert_eq!(exact.count(), 3);
        assert!(!exact.has_more);

        // One hop short: A unreached, has_more set.
        let one_short = store.upstream_closure(d, LineageQueryOptions::new().with_max_depth(2));
        assert_eq!(one_short.count(), 2);
        assert!(one_short.has_more);

        // One past the chain length: identical to exact, still no has_more.
        let one_past = store.upstream_closure(d, LineageQueryOptions::new().with_max_depth(4));
        assert_eq!(one_past.count(), 3);
        assert!(!one_past.has_more);
    }

    #[test]
    fn has_more_false_when_limit_equals_count_exactly() {
        // A has exactly 3 direct downstream facts.
        let store = LineageStore::new();
        let a = node_ref(1, 10);
        store.record(node_ref(2, 20), vec![a], ts(1)).unwrap();
        store.record(node_ref(3, 30), vec![a], ts(1)).unwrap();
        store.record(node_ref(4, 40), vec![a], ts(1)).unwrap();

        // limit == count: all returned, no has_more (traversal exhausted the
        // graph without hitting the cap mid-expansion).
        let exact = store.downstream_closure(a, LineageQueryOptions::new().with_limit(3));
        assert_eq!(exact.count(), 3);
        assert!(!exact.has_more);

        // limit one below count: has_more set.
        let under = store.downstream_closure(a, LineageQueryOptions::new().with_limit(2));
        assert_eq!(under.count(), 2);
        assert!(under.has_more);
    }

    #[test]
    fn concurrent_records_from_shared_source_no_lost_updates() {
        // N threads each record a DISTINCT derived version from one shared
        // source. The check-then-act critical section must be atomic, so every
        // record survives (write-once is not violated by a lost update) and the
        // downstream closure of the shared source counts exactly N.
        use std::sync::Arc;
        use std::thread;

        const N: u64 = 64;
        let store = Arc::new(LineageStore::new());
        let source = node_ref(1, 1);

        let mut handles = Vec::new();
        for i in 0..N {
            let store = Arc::clone(&store);
            handles.push(thread::spawn(move || {
                // Distinct derived version ids (>= 100) per thread.
                let derived = node_ref(100 + i, 100 + i);
                store
                    .record(derived, vec![source], ts(1))
                    .expect("distinct derived version records cleanly");
            }));
        }
        for h in handles {
            h.join().expect("thread panicked");
        }

        assert_eq!(store.len(), N as usize);
        let closure = store.downstream_closure(
            source,
            LineageQueryOptions::new().with_limit(N as usize + 10),
        );
        assert_eq!(closure.count(), N as usize);
        assert!(!closure.has_more);
    }

    #[test]
    fn diamond_deduplicates_shared_ancestor() {
        // D <- B <- A and D <- C <- A: A reached once, at its minimum depth.
        let store = LineageStore::new();
        let a = node_ref(1, 10);
        let b = node_ref(2, 20);
        let c = node_ref(3, 30);
        let d = node_ref(4, 40);
        store.record(b, vec![a], ts(1)).unwrap();
        store.record(c, vec![a], ts(1)).unwrap();
        store.record(d, vec![b, c], ts(1)).unwrap();

        let closure = store.upstream_closure(d, LineageQueryOptions::new());
        // B, C at depth 1; A once at depth 2.
        assert_eq!(closure.count(), 3);
        let a_entries: Vec<_> = closure
            .entries
            .iter()
            .filter(|e| e.reference == a)
            .collect();
        assert_eq!(a_entries.len(), 1);
        assert_eq!(a_entries[0].depth, 2);
    }
}
