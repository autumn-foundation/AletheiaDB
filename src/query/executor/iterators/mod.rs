//! Result Iterators
//!
//! Pull-based iterators for query execution. Each physical operator
//! has a corresponding iterator that lazily produces results.

use parking_lot::RwLock;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashSet, VecDeque};
use std::sync::Arc;

#[cfg(feature = "observability")]
use tracing;

use crate::core::error::Result;
use crate::core::graph::Node;
use crate::core::interning::GLOBAL_INTERNER;
use crate::core::property::PropertyValue;
use crate::core::vector::cosine_similarity;
use crate::core::{NodeId, Timestamp};
use crate::query::ir::{
    AggregateArg, AggregateFunc, AggregateGroupKey, AggregateSpec, Direction, Predicate,
    PredicateValue, SortKey,
};
use crate::storage::current::CurrentStorage;
use crate::storage::historical::HistoricalStorage;

use super::results::{EntityId, EntityResult, QueryRow};

/// Trait for result iteration (pull-based).
///
/// Query execution uses a pull-based iterator model, where each physical
/// operator is implemented as an iterator. Calling `next()` pulls results
/// sequentially through the pipeline.
pub trait ResultIterator: Send {
    /// Get the next result row
    fn next(&mut self) -> Option<Result<QueryRow>>;

    /// Estimate the remaining results
    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, None)
    }
}

/// Empty iterator that produces no results.
///
/// Used for query plans that evaluate to empty at planning time.
pub struct EmptyIterator;

impl ResultIterator for EmptyIterator {
    fn next(&mut self) -> Option<Result<QueryRow>> {
        None
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, Some(0))
    }
}

/// Direct node lookup iterator.
///
/// Yields nodes corresponding to a specific list of IDs. O(1) per node.
///
/// # Examples
///
/// ```ignore
/// use aletheiadb::query::executor::NodeLookupIterator;
/// use aletheiadb::storage::current::CurrentStorage;
/// use aletheiadb::core::id::NodeId;
/// use std::sync::Arc;
///
/// // Assuming `current` is a valid Arc<CurrentStorage>
/// let node_ids = vec![NodeId::new(1).unwrap(), NodeId::new(2).unwrap()];
/// let iter = NodeLookupIterator::new(node_ids, current);
/// ```
pub struct NodeLookupIterator {
    node_ids: std::vec::IntoIter<NodeId>,
    current: Arc<CurrentStorage>,
}

impl NodeLookupIterator {
    /// Initialize the iterator with a predefined list of node identifiers.
    ///
    /// # Why?
    /// This is used for `NodeLookup` physical operations where the query
    /// planner has already resolved exact node IDs (e.g., from an index or literal).
    pub fn new(node_ids: Vec<NodeId>, current: Arc<CurrentStorage>) -> Self {
        NodeLookupIterator {
            node_ids: node_ids.into_iter(),
            current,
        }
    }
}

impl ResultIterator for NodeLookupIterator {
    fn next(&mut self) -> Option<Result<QueryRow>> {
        self.node_ids.next().map(|id| {
            self.current
                .get_node(id)
                .map(|node| QueryRow::from_entity(EntityResult::Node(node)))
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.node_ids.size_hint()
    }
}

/// Label filter state for a [`NodeScanIterator`], resolved once at construction.
///
/// Resolving the requested label to its interned id up front lets the scan use
/// the fast 16-byte `node_headers` path (see [`CurrentStorage::node_has_label`])
/// instead of loading and cloning a full `Node` just to inspect its label.
enum ScanFilter {
    /// No label filter: every existing node is yielded.
    All,
    /// Yield only nodes whose interned label equals this id.
    Label(crate::core::interning::InternedString),
    /// A label was requested but has never been interned, so no node can match.
    /// The scan yields nothing (as opposed to [`ScanFilter::All`]).
    None,
}

/// Sequential node scan iterator, optionally applying a label filter.
///
/// # Memory Behavior
///
/// This iterator streams node ids lazily over the half-open range
/// `[0, max_id)`, where `max_id` is current storage's insert-maintained node-id
/// high-water-mark (see [`CurrentStorage::get_max_node_id`]), captured once at
/// construction. It never materializes the full id set, so each `next()` call
/// allocates O(1) regardless of graph size. This makes a full scan immune to
/// the out-of-memory failure that eager `Vec<NodeId>` materialization caused on
/// very large graphs.
///
/// Ids with no live node (gaps from deletion, or ids reserved but never used)
/// are skipped cheaply: both label and unfiltered scans reject them via the
/// compact 16-byte header fast path before loading any full node.
///
/// # Performance
///
/// The iterator picks one of two strategies at construction (Issue #3422),
/// based on O(1) reads of the live node count and the id high-water mark:
///
/// - **Sweep** (dense / huge-live / tiny id spaces): streams ids over
///   `[0, max_id)` exactly as PR #3418 did. Resident memory is O(1), time is
///   O(max_id). Ids with no live node are skipped cheaply via the 16-byte
///   header fast path. This is optimal when the id space is densely populated,
///   since `max_id` is close to the live count.
/// - **Paged** (sparse id spaces): pages the live keys of `node_headers` in
///   ascending-id chunks of `page_size` (K), yielding each page's ids before
///   fetching the next. Dead ids (deletion tombstones, reserved-but-unused
///   ids, post-compaction gaps) are **never visited**, so time is O(live)
///   rather than O(max_id), while resident memory stays O(K) -- it never
///   materializes the whole id set (the OOM vector PR #3418 removed).
///
/// The sparse/paged path is chosen only when it does **comfortably** less work
/// than the sweep -- at least 2x less (`2 * live * ceil(live/K) < max_id`) --
/// and the live count is bounded, so dense *and near-dense* graphs never regress
/// (a graph with only a few deletion gaps stays on the sweep rather than paying
/// a heap-sort to dodge them), while a once-huge-now-tiny graph (e.g. 1B ids
/// ever allocated, 1K now live) recovers O(live) full-scan time instead of
/// sweeping ~1B dead ids. A fully bulk-deleted graph (0 live, huge `max_id`) is
/// special-cased to an O(1) exhausted scan rather than an O(max_id) sweep of all
/// dead ids. Node ids are monotonic and never reused, so `max_id` is the
/// high-water mark of ids ever allocated, not the live count -- which is
/// exactly why the naive sweep degraded.
///
/// # Concurrency
///
/// Neither strategy holds a DashMap shard lock across a yield. The sweep
/// acquires a shard lock only for the duration of a single `node_has_label` /
/// `contains_node` / `get_node` call. The paged path collects each page under
/// brief per-shard locks that are all released before any row is yielded (see
/// [`CurrentStorage::collect_node_id_page`]); between yields no lock is held.
/// So the iterator cannot deadlock against concurrent writers and does not
/// violate the crate's lock acquisition order. Both the `max_id` sweep bound
/// and the paged key snapshot are relaxed (non-isolated): nodes created or
/// deleted after a page is captured may or may not be observed, which is the
/// expected semantics for an unsynchronized full scan.
///
/// # Examples
///
/// ```ignore
/// use aletheiadb::query::executor::NodeScanIterator;
/// use aletheiadb::storage::current::CurrentStorage;
/// use std::sync::Arc;
///
/// // Assuming `current` is a valid Arc<CurrentStorage>
/// let iter = NodeScanIterator::new(Some("Person".to_string()), current);
/// ```
pub struct NodeScanIterator {
    filter: ScanFilter,
    current: Arc<CurrentStorage>,
    /// Which scanning strategy is in flight, plus its live cursor state.
    mode: ScanMode,
    /// Page size (K) for the paged strategy; ignored by the sweep.
    page_size: usize,
    /// Instrumentation: number of candidate ids examined so far. In the sweep
    /// path this counts every id in `[0, max_id)` visited (including dead ids
    /// skipped by the header fast paths). In the paged path it counts every live
    /// candidate the per-page enumeration inspected across all pages -- i.e. the
    /// real `live * pages` re-scan cost, **not** just the live ids materialized
    /// (a single page examines `live` candidates to retain up to `K`, so
    /// counting only retained ids would understate a multi-page scan by a factor
    /// of `pages`). It is the test/bench proxy for scan work, exposed via
    /// [`Self::work_units`], and lets a test assert that a full scan's cost
    /// tracks the live node count rather than `max_id`.
    work_units: u64,
}

/// Default page size (K) for the chunked `node_headers` scan (Issue #3422).
///
/// Bounds resident memory of the paged strategy to O(K) node ids while keeping
/// per-page overhead amortized. Tunable in tests/benches via
/// [`NodeScanIterator::with_strategy`].
const DEFAULT_NODE_SCAN_PAGE_SIZE: usize = 4096;

/// Above this live-node count the paged strategy's per-page re-scan cost stops
/// paying for itself, so the scan stays on the memory-flat sweep even for a
/// sparse id space. See [`NodeScanIterator::prefer_paged`].
const MAX_PAGED_LIVE_NODES: u64 = 2_000_000;

/// Full-scan strategy selector (Issue #3422). `Auto` picks sweep vs paged from
/// the live count and id high-water mark; the `Force*` variants are test/bench
/// hooks to exercise a specific path deterministically.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanStrategy {
    /// Choose sweep or paged automatically from live count vs `max_id`.
    Auto,
    /// Always use the dense `[0, max_id)` sweep (PR #3418 behavior).
    ForceSweep,
    /// Always use the chunked `node_headers` keyset paging (Issue #3422).
    ForcePaged,
}

/// Internal scan strategy state.
enum ScanMode {
    /// Dense streaming sweep of `[0, max_id)` (PR #3418).
    Sweep { current_id: u64, max_id: u64 },
    /// Chunked keyset paging over live `node_headers` keys (Issue #3422).
    ///
    /// `buffer` drains the current ascending-id page; `cursor` is the last id
    /// yielded (the keyset boundary for the next page); `exhausted` is set once
    /// a short/empty page proves the live key space after the cursor is drained.
    Paged {
        buffer: std::vec::IntoIter<NodeId>,
        cursor: Option<u64>,
        exhausted: bool,
    },
}

impl NodeScanIterator {
    /// Create a new NodeScanIterator, auto-selecting the scan strategy.
    ///
    /// See the type-level `# Performance` docs for how the sweep vs paged
    /// strategy is chosen; behavior and results are identical to the prior
    /// sweep-only iterator, only the traversal cost differs.
    pub fn new(label: Option<String>, current: Arc<CurrentStorage>) -> Self {
        Self::with_strategy(
            label,
            current,
            ScanStrategy::Auto,
            DEFAULT_NODE_SCAN_PAGE_SIZE,
        )
    }

    /// Construct with an explicit [`ScanStrategy`] and page size.
    ///
    /// Public but hidden: `new` (auto strategy, default page size) is the
    /// supported entry point. This exists so tests/benches can pin a specific
    /// strategy and page size to measure before/after scan cost deterministically.
    #[doc(hidden)]
    pub fn with_strategy(
        label: Option<String>,
        current: Arc<CurrentStorage>,
        strategy: ScanStrategy,
        page_size: usize,
    ) -> Self {
        // Resolve the label to an interned id exactly once. A requested-but-
        // unknown label short-circuits to `None` so the scan yields nothing,
        // rather than degrading to an unfiltered scan.
        let filter = match label {
            Option::None => ScanFilter::All,
            Some(ref l) => match GLOBAL_INTERNER.get_id(l) {
                Some(id) => ScanFilter::Label(id),
                Option::None => ScanFilter::None,
            },
        };

        let page_size = page_size.max(1);

        let mode = if matches!(filter, ScanFilter::None) {
            // No node can ever match; an empty sweep drains immediately without
            // touching storage.
            ScanMode::Sweep {
                current_id: 0,
                max_id: 0,
            }
        } else {
            // Snapshot the id upper bound once. `get_max_node_id` returns the
            // index's insert-maintained high-water-mark (`max id ever inserted
            // + 1`) rather than any id generator, so a sweep of `[0, max_id)`
            // covers every node present regardless of which generator allocated
            // its id.
            let max_id = current.get_max_node_id();
            match strategy {
                ScanStrategy::ForceSweep => ScanMode::Sweep {
                    current_id: 0,
                    max_id,
                },
                ScanStrategy::ForcePaged => Self::new_paged_mode(),
                ScanStrategy::Auto => {
                    Self::choose_mode(current.node_count() as u64, max_id, page_size)
                }
            }
        };

        NodeScanIterator {
            filter,
            current,
            mode,
            page_size,
            work_units: 0,
        }
    }

    /// Fresh, empty paged-mode state (cursor at the start, nothing buffered).
    fn new_paged_mode() -> ScanMode {
        ScanMode::Paged {
            buffer: Vec::new().into_iter(),
            cursor: None,
            exhausted: false,
        }
    }

    /// Pick sweep vs paged from O(1) counters (Issue #3422).
    fn choose_mode(live: u64, max_id: u64, page_size: usize) -> ScanMode {
        // Zero live nodes with a non-trivial id high-water mark is the exact
        // pathology this PR targets: e.g. a fully bulk-deleted graph where
        // `node_count() == 0` but `max_id` is still the (possibly huge)
        // high-water mark of ids ever allocated. A `[0, max_id)` sweep would
        // examine every dead id just to yield nothing -- O(max_id). Start in an
        // already-exhausted paged mode so the scan does O(1) work instead.
        if live == 0 {
            return ScanMode::Paged {
                buffer: Vec::new().into_iter(),
                cursor: None,
                exhausted: true,
            };
        }
        if Self::prefer_paged(live, max_id, page_size) {
            Self::new_paged_mode()
        } else {
            ScanMode::Sweep {
                current_id: 0,
                max_id,
            }
        }
    }

    /// Whether the chunked paged strategy is expected to beat the dense sweep.
    ///
    /// The sweep probes `max_id` ids. The paged strategy re-scans the live
    /// header set once per page, so it costs about `live * ceil(live / K)`
    /// header reads across `ceil(live / K)` pages. Prefer paging only when that
    /// paged cost is comfortably cheaper than the sweep -- specifically at least
    /// **2x** cheaper (`2 * paged_cost < max_id`) and the live count is bounded.
    ///
    /// The 2x margin is deliberate: without it, a *near-dense* graph (say
    /// `live == max_id - few`, a handful of deletion gaps) would page purely to
    /// dodge a few cheap gap-skips, paying a heap-sort and per-page re-scan for
    /// no real win -- contradicting the "strictly less work" intent. Requiring a
    /// clear margin keeps dense and near-dense graphs on the memory-flat sweep
    /// and reserves paging for genuinely sparse id spaces (a once-huge-now-tiny
    /// graph), where it recovers O(live) time. The live bound additionally keeps
    /// a huge-live graph -- where the sweep is already ~O(live) and memory-flat
    /// -- off the quadratic re-scan. (`live == 0` is handled earlier in
    /// [`Self::choose_mode`] as an O(1) exhausted scan, so it never reaches
    /// here.)
    fn prefer_paged(live: u64, max_id: u64, page_size: usize) -> bool {
        if live == 0 || live > MAX_PAGED_LIVE_NODES {
            return false;
        }
        let k = page_size.max(1) as u128;
        let pages = (live as u128).div_ceil(k);
        let paged_cost = (live as u128).saturating_mul(pages);
        // Require paged to be at least 2x cheaper than the sweep (see above).
        paged_cost.saturating_mul(2) < max_id as u128
    }

    /// Total candidate ids examined so far (see the [`work_units`] field docs).
    ///
    /// Test/bench instrumentation, not part of the query contract.
    ///
    /// [`work_units`]: Self::work_units
    #[doc(hidden)]
    pub fn work_units(&self) -> u64 {
        self.work_units
    }

    /// Collect one page of live node ids honoring the active label filter.
    ///
    /// Returns the page plus the number of live candidates the enumeration
    /// **examined** to build it (the per-page re-scan cost, `~live`), which the
    /// caller folds into `work_units` so the paged work proxy reflects the true
    /// `live * pages` cost rather than only the materialized ids.
    ///
    /// Static (no `self`) so the caller can borrow `current`/`filter`
    /// immutably alongside a mutable borrow of `self.mode` (disjoint fields).
    fn collect_page(
        current: &CurrentStorage,
        filter: &ScanFilter,
        after: Option<u64>,
        k: usize,
    ) -> (Vec<NodeId>, u64) {
        match filter {
            ScanFilter::All => current.collect_node_id_page_counted(after, k, None),
            ScanFilter::Label(label_id) => {
                current.collect_node_id_page_counted(after, k, Some(*label_id))
            }
            ScanFilter::None => (Vec::new(), 0),
        }
    }

    /// Refill the paged buffer with the next chunk of live node ids.
    ///
    /// Returns `false` when the scan is exhausted (no more live ids after the
    /// cursor); otherwise the buffer holds at least one id.
    fn refill_page(&mut self) -> bool {
        let (after, already_exhausted) = match &self.mode {
            ScanMode::Paged {
                cursor, exhausted, ..
            } => (*cursor, *exhausted),
            ScanMode::Sweep { .. } => return false,
        };
        if already_exhausted {
            return false;
        }

        let k = self.page_size;
        // Disjoint borrows: `current`/`filter` immutable, `mode` untouched here.
        let (page, examined) = Self::collect_page(&self.current, &self.filter, after, k);
        // The per-page enumeration examined `examined` live candidates; that is
        // the real cost this page paid, so fold it into the work proxy here (at
        // page granularity) rather than per yielded id.
        self.work_units += examined;

        match &mut self.mode {
            ScanMode::Paged {
                buffer,
                cursor,
                exhausted,
            } => {
                if page.is_empty() {
                    *exhausted = true;
                    return false;
                }
                // A short page means the live key space after the cursor is
                // drained; mark exhausted so the next drain stops without a
                // fruitless re-scan.
                if page.len() < k {
                    *exhausted = true;
                }
                *cursor = page.last().map(|id| id.as_u64());
                *buffer = page.into_iter();
                true
            }
            ScanMode::Sweep { .. } => false,
        }
    }

    /// Advance the dense `[0, max_id)` sweep (PR #3418 behavior).
    fn next_sweep(&mut self) -> Option<Result<QueryRow>> {
        loop {
            let id_val = match &mut self.mode {
                ScanMode::Sweep { current_id, max_id } => {
                    if *current_id >= *max_id {
                        return None;
                    }
                    let v = *current_id;
                    *current_id += 1;
                    v
                }
                ScanMode::Paged { .. } => return None,
            };
            self.work_units += 1;

            // Fast path: reject non-matching labels via the compact node header
            // before ever loading the full node.
            if let ScanFilter::Label(label_id) = self.filter
                && !self.current.node_has_label(id_val, label_id)
            {
                continue;
            }

            // Fast path: for an unfiltered scan, skip gaps in the sparse id
            // space with a cheap O(1) header existence check.
            if matches!(self.filter, ScanFilter::All) && !self.current.contains_node(id_val) {
                continue;
            }

            let id = match NodeId::new(id_val) {
                Ok(id) => id,
                Err(_) => continue,
            };

            match self.current.get_node(id) {
                Ok(node) => return Some(Ok(QueryRow::from_entity(EntityResult::Node(node)))),
                // Sparse id space: skip ids with no live node.
                Err(crate::core::error::Error::Storage(
                    crate::core::error::StorageError::NodeNotFound(_),
                )) => continue,
                Err(e) => return Some(Err(e)),
            }
        }
    }

    /// Advance the chunked `node_headers` keyset paging (Issue #3422).
    fn next_paged(&mut self) -> Option<Result<QueryRow>> {
        loop {
            // Pull the next id from the current page, refilling as it drains.
            let id_val = loop {
                let next = match &mut self.mode {
                    ScanMode::Paged { buffer, .. } => buffer.next(),
                    ScanMode::Sweep { .. } => return None,
                };
                match next {
                    Some(id) => break id.as_u64(),
                    None => {
                        if !self.refill_page() {
                            return None;
                        }
                    }
                }
            };

            // Note: work is accounted per-page in `refill_page` (the enumeration
            // cost), not per yielded id, so the paged work proxy reflects the
            // true `live * pages` re-scan cost. Do not increment here.

            let id = match NodeId::new(id_val) {
                Ok(id) => id,
                Err(_) => continue,
            };

            match self.current.get_node(id) {
                Ok(node) => return Some(Ok(QueryRow::from_entity(EntityResult::Node(node)))),
                // The id was live when the page was captured but was deleted
                // before this load; skip it (relaxed snapshot semantics).
                Err(crate::core::error::Error::Storage(
                    crate::core::error::StorageError::NodeNotFound(_),
                )) => continue,
                Err(e) => return Some(Err(e)),
            }
        }
    }
}

impl ResultIterator for NodeScanIterator {
    fn next(&mut self) -> Option<Result<QueryRow>> {
        // A label that was never interned can match no node.
        if matches!(self.filter, ScanFilter::None) {
            return None;
        }

        match self.mode {
            ScanMode::Sweep { .. } => self.next_sweep(),
            ScanMode::Paged { .. } => self.next_paged(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        if matches!(self.filter, ScanFilter::None) {
            return (0, Some(0));
        }
        match &self.mode {
            // Upper bound only: the range may contain gaps that are skipped.
            ScanMode::Sweep { current_id, max_id } => {
                (0, Some(max_id.saturating_sub(*current_id) as usize))
            }
            // Paged: the remaining live count is not cheaply known here.
            ScanMode::Paged { .. } => (0, None),
        }
    }
}

/// Iterator for vector search results.
///
/// # Context
/// Transforms raw `(NodeId, score)` pairs from a vector search operation
/// (like `HnswSearch`) into fully populated `QueryRow` results containing
/// the actual `Node` entities.
///
/// # Details
/// Performs lazy, row-by-row lookups against `CurrentStorage`. This ensures that
/// node properties are only materialized in memory when `next()` is explicitly called.
///
/// # Panics
/// Does not panic. If a node ID from the search results is no longer found in storage
/// (e.g., due to a concurrent deletion), it returns an `Err` which is yielded to the caller.
///
/// # Examples
///
/// ```rust
/// # use std::sync::Arc;
/// # use aletheiadb::storage::current::CurrentStorage;
/// # use aletheiadb::core::id::NodeId;
/// # use aletheiadb::query::executor::VectorResultIterator;
/// # use aletheiadb::query::executor::ResultIterator;
/// #
/// # let current = Arc::new(CurrentStorage::new());
/// # let node_id = current.create_node("Doc", aletheiadb::core::PropertyMapBuilder::new().build()).unwrap();
/// // Raw results from an HNSW index search
/// let raw_results = vec![(node_id, 0.95), (node_id, 0.85)];
///
/// let mut iter = VectorResultIterator::new(raw_results, current);
///
/// while let Some(result) = iter.next() {
///     let row = result.expect("Node should exist");
///     println!("Found node {:?} with similarity score: {}", row.entity, row.score.unwrap());
/// }
/// ```
pub struct VectorResultIterator {
    results: std::vec::IntoIter<(NodeId, f32)>,
    current: Arc<CurrentStorage>,
}

impl VectorResultIterator {
    /// Create a new VectorResultIterator.
    pub fn new(results: Vec<(NodeId, f32)>, current: Arc<CurrentStorage>) -> Self {
        VectorResultIterator {
            results: results.into_iter(),
            current,
        }
    }
}

impl ResultIterator for VectorResultIterator {
    fn next(&mut self) -> Option<Result<QueryRow>> {
        self.results.next().map(|(node_id, score)| {
            self.current
                .get_node(node_id)
                .map(|node| QueryRow::with_score(EntityResult::Node(node), score))
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.results.size_hint()
    }
}

/// Reconstruct a node as it existed at `(valid_time, transaction_time)` from an
/// already-locked historical storage (Issue #356).
///
/// This is the single, flat implementation of the temporal reconstruction path
/// shared by [`TemporalNodeIterator`], [`BatchTemporalNodeIterator`], and
/// [`TemporalNodeScanIterator`], replacing the previously duplicated (and
/// originally deeply nested) per-iterator logic:
///
/// 1. Find the version valid at the requested bi-temporal point.
/// 2. Retrieve the version metadata (validates that
///    `find_node_version_at_time` only returns existing version IDs).
/// 3. Reconstruct properties from the anchor+delta compression.
/// 4. Build a `Node` with the historical label and properties.
///
/// The caller decides the locking strategy (per-node vs batch) and passes the
/// already-locked storage, so this helper never affects lock semantics.
///
/// # Errors
///
/// - [`TemporalError::NodeNotFoundAtTime`](crate::core::error::TemporalError::NodeNotFoundAtTime)
///   if no version exists at the requested time point
/// - [`TemporalError::VersionNotFound`](crate::core::error::TemporalError::VersionNotFound)
///   if version metadata is missing (data inconsistency)
/// - Any error from property reconstruction
fn reconstruct_node_at(
    historical: &HistoricalStorage,
    node_id: NodeId,
    valid_time: Timestamp,
    transaction_time: Timestamp,
) -> Result<Node> {
    // Step 1: Find the version valid at the requested time
    let version_id = historical
        .find_node_version_at_time(node_id, valid_time, transaction_time)
        .ok_or(crate::core::error::TemporalError::NodeNotFoundAtTime {
            node_id,
            valid_time,
            transaction_time,
        })?;

    // Step 2: Get the version metadata (also validates the invariant that
    // find_node_version_at_time only returns existing version IDs)
    let version = historical.get_node_version(version_id).ok_or(
        crate::core::error::TemporalError::VersionNotFound(version_id),
    )?;

    // Step 3: Reconstruct the properties from the version
    let properties = historical.reconstruct_node_properties(version_id)?;

    // Step 4: Build and return the node with the historical data
    Ok(Node::new(node_id, version.label, properties, version_id))
}

/// Iterator for temporal node lookups.
///
/// # Context
/// Reconstructs nodes at a specific point in bi-temporal time by querying
/// historical storage. It transforms a sequence of `NodeId`s into fully
/// populated `Node`s representing their exact state at `(valid_time, transaction_time)`.
///
/// # Details
/// The reconstruction process:
/// 1. Finds the version valid at the requested bi-temporal point.
/// 2. Reconstructs properties from the version using the anchor+delta compression strategy.
/// 3. Returns a `Node` with the historical label and properties.
///
/// This iterator acquires a brief, per-node read lock on `HistoricalStorage`.
/// For bulk queries where lock overhead is a concern, use [`BatchTemporalNodeIterator`] instead.
///
/// # Panics
/// Does not panic. If a node or version is not found, or if property reconstruction fails,
/// it returns an `Err(TemporalError)`.
///
/// # Examples
///
/// ```rust
/// # use std::sync::Arc;
/// # use parking_lot::RwLock;
/// # use aletheiadb::storage::historical::HistoricalStorage;
/// # use aletheiadb::core::id::NodeId;
/// # use aletheiadb::core::temporal::time;
/// # use aletheiadb::query::executor::TemporalNodeIterator;
/// # use aletheiadb::query::executor::ResultIterator;
/// #
/// # let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
/// # let node_id = NodeId::new(1).unwrap();
/// let now = time::now();
/// let node_ids = vec![node_id];
///
/// let mut iter = TemporalNodeIterator::new(
///     node_ids,
///     now, // valid_time
///     now, // transaction_time
///     historical
/// );
///
/// // Iterate over the historical states
/// while let Some(result) = iter.next() {
///     // Handle potential TemporalError if node didn't exist at `now`
///     if let Ok(row) = result {
///         println!("Historical node state: {:?}", row.entity);
///     }
/// }
/// ```
pub struct TemporalNodeIterator {
    node_ids: std::vec::IntoIter<NodeId>,
    valid_time: Timestamp,
    transaction_time: Timestamp,
    historical: Arc<RwLock<HistoricalStorage>>,
}

impl TemporalNodeIterator {
    /// Create a new TemporalNodeIterator.
    pub fn new(
        node_ids: Vec<NodeId>,
        valid_time: Timestamp,
        transaction_time: Timestamp,
        historical: Arc<RwLock<HistoricalStorage>>,
    ) -> Self {
        TemporalNodeIterator {
            node_ids: node_ids.into_iter(),
            valid_time,
            transaction_time,
            historical,
        }
    }
}

impl ResultIterator for TemporalNodeIterator {
    fn next(&mut self) -> Option<Result<QueryRow>> {
        self.node_ids.next().map(|id| {
            // Acquire read lock on historical storage (per-node)
            // For bulk queries, use BatchTemporalNodeIterator instead
            let historical = self.historical.read();

            // Reconstruct the node at the requested bi-temporal point (Issue #356)
            let node =
                reconstruct_node_at(&historical, id, self.valid_time, self.transaction_time)?;

            Ok(QueryRow::from_entity(EntityResult::Node(node)).at_time(self.valid_time))
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.node_ids.size_hint()
    }
}

/// Batch temporal node iterator for bulk queries.
///
/// # Context
/// An optimized alternative to [`TemporalNodeIterator`] for reconstructing
/// many historical nodes simultaneously. It minimizes lock contention by acquiring
/// a single read lock on `HistoricalStorage`, processing all nodes, and releasing it immediately.
///
/// # Details
/// **Performance**: Use this for bulk queries (>100 nodes) where lock acquisition
/// overhead is significant.
///
/// **Trade-off**: Collects all results eagerly into memory during construction.
/// This requires more upfront memory allocation (O(n)) but avoids per-node lock
/// overhead and allows writer threads to proceed without waiting for the entire
/// iteration to complete.
///
/// # Panics
/// Does not panic. Returns an error during construction if the `HistoricalStorage` lock is poisoned.
///
/// # Examples
///
/// ```rust
/// # use std::sync::Arc;
/// # use parking_lot::RwLock;
/// # use aletheiadb::storage::historical::HistoricalStorage;
/// # use aletheiadb::core::id::NodeId;
/// # use aletheiadb::core::temporal::time;
/// # use aletheiadb::query::executor::BatchTemporalNodeIterator;
/// # use aletheiadb::query::executor::ResultIterator;
/// #
/// # let historical = Arc::new(RwLock::new(HistoricalStorage::new()));
/// # let node_ids = vec![NodeId::new(1).unwrap(), NodeId::new(2).unwrap()];
/// let point_in_time = time::now();
///
/// // Lock is acquired, nodes are processed, and lock is released during `new()`
/// let mut batch_iter = BatchTemporalNodeIterator::new(
///     node_ids,
///     point_in_time,
///     point_in_time,
///     historical
/// ).expect("Lock should not be poisoned");
///
/// // Iteration is lock-free and pulls from memory
/// while let Some(Ok(row)) = batch_iter.next() {
///     println!("Bulk historical data: {:?}", row.entity);
/// }
/// ```
pub struct BatchTemporalNodeIterator {
    results: std::vec::IntoIter<Result<QueryRow>>,
}

impl BatchTemporalNodeIterator {
    /// Create a new batch temporal node iterator.
    ///
    /// Initialize an iterator that resolves multiple historical nodes simultaneously.
    ///
    /// # Why?
    /// Unlike the standard `TemporalNodeIterator`, this optimizes lock acquisition
    /// by grabbing the read lock once for the entire batch. This prevents lock contention
    /// on highly active graphs during deep historical traversals.
    ///
    /// Acquires the historical storage lock once, reconstructs all nodes,
    /// then releases the lock and returns the iterator over results.
    ///
    /// # Errors
    /// Returns an error if the historical storage lock is poisoned.
    pub fn new(
        node_ids: Vec<NodeId>,
        valid_time: Timestamp,
        transaction_time: Timestamp,
        historical: Arc<RwLock<HistoricalStorage>>,
    ) -> Result<Self> {
        // Acquire lock once for all nodes
        let guard = historical.read();

        // Reconstruct all nodes while holding the lock
        let results: Vec<Result<QueryRow>> = node_ids
            .into_iter()
            .map(|id| {
                // Reconstruct the node at the requested bi-temporal point (Issue #356)
                let node = reconstruct_node_at(&guard, id, valid_time, transaction_time)?;

                Ok(QueryRow::from_entity(EntityResult::Node(node)).at_time(valid_time))
            })
            .collect();

        // Lock is automatically released here when `guard` goes out of scope

        Ok(BatchTemporalNodeIterator {
            results: results.into_iter(),
        })
    }
}

impl ResultIterator for BatchTemporalNodeIterator {
    fn next(&mut self) -> Option<Result<QueryRow>> {
        self.results.next()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.results.size_hint()
    }
}

/// Iterator for temporal node lookups with optional label filtering.
///
/// This iterator addresses the deep nesting issue (#356) by extracting the
/// filtering logic into well-defined helper methods:
///
/// - `get_temporal_version()` - Retrieves a node at a specific point in bi-temporal time
/// - `apply_label_filter()` - Checks if a node matches the optional label filter
/// - `filter_node()` - Orchestrates the filtering logic with maximum 2-3 levels of nesting
///
/// ## Design Rationale
///
/// Instead of deeply nested conditionals (8+ levels), this design:
/// 1. Separates concerns into small, focused methods
/// 2. Keeps each method at 2-3 levels of nesting maximum
/// 3. Makes each component independently testable
/// 4. Improves readability and maintainability
///
/// ## Lock Duration Trade-off
///
/// The `next()` method holds the historical read lock for the entire iteration
/// loop until a matching node is found. This is intentional:
/// - **Advantage**: Avoids lock thrashing (acquiring/releasing on every node)
/// - **Trade-off**: For large result sets with many filtered-out nodes, the lock
///   may be held longer, potentially increasing writer latency
///
/// For bulk queries where this is a concern, consider using `BatchTemporalNodeIterator`
/// which processes all nodes upfront and releases the lock immediately.
///
/// ## Example
///
/// ```ignore
/// let iter = TemporalNodeScanIterator::new(
///     node_ids,
///     valid_time,
///     transaction_time,
///     historical,
///     Some("Person".to_string()), // Optional label filter
/// );
///
/// for result in iter {
///     // Only Person nodes at the specified time point
/// }
/// ```
pub struct TemporalNodeScanIterator {
    node_ids: std::vec::IntoIter<NodeId>,
    valid_time: Timestamp,
    transaction_time: Timestamp,
    historical: Arc<RwLock<HistoricalStorage>>,
    /// Optional label filter - if Some, only nodes with matching label are returned
    label_filter: Option<String>,
    /// Pre-computed interned ID of the label filter for efficient comparison.
    /// Avoids repeated hashmap lookups in apply_label_filter().
    interned_label_filter: Option<crate::core::interning::InternedString>,
    /// When `true`, a candidate that has no version at the requested
    /// bi-temporal point (or whose version metadata is missing) is silently
    /// skipped instead of surfacing an error. This is the correct semantics
    /// for a point-in-time label *scan* (Issues #550/#551): the candidate set
    /// is "every node ever versioned", most of which need not exist at the
    /// queried instant. The per-node lookup path (default `false`) keeps
    /// propagating the error, since there the caller explicitly named the id.
    skip_missing: bool,
}

impl TemporalNodeScanIterator {
    /// Initialize a scanning iterator that searches historical storage.
    ///
    /// # Why?
    /// This provides a fallback mechanism to find nodes in the past when temporal
    /// indexes are either unavailable or explicitly bypassed.
    ///
    /// # Arguments
    ///
    /// * `node_ids` - The node IDs to iterate over
    /// * `valid_time` - The valid time for temporal reconstruction
    /// * `transaction_time` - The transaction time for temporal reconstruction
    /// * `historical` - Reference to historical storage
    /// * `label_filter` - Optional label to filter nodes by
    pub fn new(
        node_ids: Vec<NodeId>,
        valid_time: Timestamp,
        transaction_time: Timestamp,
        historical: Arc<RwLock<HistoricalStorage>>,
        label_filter: Option<String>,
    ) -> Self {
        // Pre-compute the interned label ID once during construction
        // to avoid repeated hashmap lookups during iteration
        let interned_label_filter = label_filter
            .as_ref()
            .and_then(|label| GLOBAL_INTERNER.get_id(label));

        TemporalNodeScanIterator {
            node_ids: node_ids.into_iter(),
            valid_time,
            transaction_time,
            historical,
            label_filter,
            interned_label_filter,
            skip_missing: false,
        }
    }

    /// Enable point-in-time *scan* semantics: candidates absent at the queried
    /// bi-temporal point are skipped rather than raising
    /// [`TemporalError::NodeNotFoundAtTime`](crate::core::error::TemporalError::NodeNotFoundAtTime).
    ///
    /// Used by the Cypher/AQL label-scan `AS OF` path (Issues #550/#551), where
    /// the candidate set is every ever-versioned node and most of them simply
    /// did not exist at the instant being asked about.
    #[must_use]
    pub fn skipping_missing(mut self) -> Self {
        self.skip_missing = true;
        self
    }

    /// Retrieve the temporal version of a node at the configured time point.
    ///
    /// Thin wrapper over the shared [`reconstruct_node_at`] helper (Issue #356),
    /// binding this iterator's configured `(valid_time, transaction_time)`.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - No version exists at the specified time point
    /// - Version metadata is missing (data inconsistency)
    /// - Property reconstruction fails
    pub(crate) fn get_temporal_version(
        &self,
        node_id: NodeId,
        guard: &parking_lot::RwLockReadGuard<'_, HistoricalStorage>,
    ) -> Result<Node> {
        reconstruct_node_at(guard, node_id, self.valid_time, self.transaction_time)
    }

    /// Check if a node passes the label filter.
    ///
    /// Returns `true` if:
    /// - No label filter is configured (all nodes pass)
    /// - The node's label matches the filter
    ///
    /// Returns `false` if:
    /// - The node's label doesn't match the filter
    /// - The filter label doesn't exist in the interner (no nodes can match)
    ///
    /// Uses the pre-computed interned label ID for O(1) comparison.
    #[inline]
    pub(crate) fn apply_label_filter(&self, node: &Node) -> bool {
        match (&self.label_filter, self.interned_label_filter) {
            (None, _) => true,        // No filter, all nodes pass
            (Some(_), None) => false, // Filter label doesn't exist, no nodes match
            (Some(_), Some(filter_id)) => filter_id == node.label,
        }
    }

    /// Orchestrate the filtering logic for a single node.
    ///
    /// This method combines temporal reconstruction with label filtering
    /// while maintaining flat control flow (2-3 levels of nesting max).
    ///
    /// # Returns
    ///
    /// - `Some(Ok(QueryRow))` - Node exists at time point and passes label filter
    /// - `Some(Err(...))` - Node lookup failed (error should be propagated)
    /// - `None` - Node exists but doesn't pass label filter (skip to next)
    pub(crate) fn filter_node(
        &self,
        node_id: NodeId,
        guard: &parking_lot::RwLockReadGuard<'_, HistoricalStorage>,
    ) -> Option<Result<QueryRow>> {
        // Step 1: Get the temporal version
        let node = match self.get_temporal_version(node_id, guard) {
            Ok(n) => n,
            Err(e) => return Some(Err(e)),
        };

        // Step 2: Apply label filter
        if !self.apply_label_filter(&node) {
            return None; // Skip this node
        }

        // Step 3: Build and return the query row
        Some(Ok(
            QueryRow::from_entity(EntityResult::Node(node)).at_time(self.valid_time)
        ))
    }
}

impl ResultIterator for TemporalNodeScanIterator {
    fn next(&mut self) -> Option<Result<QueryRow>> {
        // Acquire read lock once for the duration of finding the next valid node
        let guard = self.historical.read();

        loop {
            let node_id = self.node_ids.next()?;

            match self.filter_node(node_id, &guard) {
                // In scan mode, a candidate absent at the queried instant is not
                // an error -- it simply isn't in the result set. Skip it and
                // keep scanning instead of aborting the whole query.
                Some(Err(e)) if self.skip_missing && is_missing_at_time(&e) => continue,
                Some(result) => return Some(result), // Found valid node or error
                None => continue,                    // Label filter didn't match, try next
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let (_lower, upper) = self.node_ids.size_hint();
        // When a label filter is active, this iterator may skip node IDs,
        // so we cannot safely use the underlying lower bound.
        // Upper bound remains valid as we can't return more than remaining IDs.
        if self.label_filter.is_some() {
            (0, upper)
        } else {
            // No label filtering: all remaining node_ids will be yielded
            // (assuming they exist in storage at the requested time point).
            self.node_ids.size_hint()
        }
    }
}

/// Returns `true` for the "this entity has no version at the queried
/// bi-temporal point" family of errors, which a point-in-time *scan*
/// (Issues #550/#551) treats as "not in the result set" rather than a hard
/// failure. A per-node lookup, where the caller explicitly named the id, keeps
/// propagating these as errors.
fn is_missing_at_time(err: &crate::core::error::Error) -> bool {
    use crate::core::error::{Error, TemporalError};
    matches!(
        err,
        Error::Temporal(TemporalError::NodeNotFoundAtTime { .. })
            | Error::Temporal(TemporalError::VersionNotFound(_))
    )
}

/// Iterator for valid-time range label scans (`BETWEEN ... AND ...`, Issue #552).
///
/// # Semantics
///
/// Given a candidate set of node ids and a valid-time range `[valid_from,
/// valid_to)`, this yields every version of each candidate that is **believed
/// at** the observed `transaction_time` (its transaction interval contains TT
/// -- the same predicate a point-in-time `AS OF` uses) **and** whose valid-time
/// interval *overlaps* the range, optionally filtered by `label`.
///
/// Semantically this is an **as-of-TT snapshot across a valid-time range**: it
/// equals the union, over every valid instant `v` in `[valid_from, valid_to)`,
/// of `AS OF (v, TT)`, deduplicated by version. Earlier valid segments that are
/// no longer believed at TT (superseded by a later transaction-time write, or
/// closed by a retraction) are **excluded**, consistent with `AS OF`, so no
/// stale beliefs and no duplicate rows appear.
///
/// In the current storage model each forward write closes the prior version's
/// transaction interval (transaction-time supersession), so at a fixed TT at
/// most one version per node is believed; a node therefore contributes at most
/// one row -- its believed-at-TT state -- and only when that state's valid
/// interval overlaps the range. (Were the store to retain multiple co-current
/// valid segments, this would naturally emit one row per in-range version.)
/// Because a single node can have several versions overlapping the range, this
/// iterator may emit **multiple rows per node** -- one per overlapping version
/// -- which is the openCypher-ish reading of a `BETWEEN` range query.
///
/// # Eager reconstruction
///
/// Like [`BatchTemporalNodeIterator`], all rows are reconstructed eagerly under
/// a single historical read lock during construction. A range scan is inherently
/// heavier than a point lookup, and eager collection keeps the lock held for a
/// bounded, predictable window instead of across the entire (lazy) drain.
///
/// # Ordering
///
/// At a fixed `transaction_time` at most one version per node is believed (see
/// the type-level note above), so a node contributes at most one row; multiple
/// rows arise only across DISTINCT nodes. Nodes are emitted in the order of the
/// supplied candidate list. The per-node selection is still sorted
/// oldest-`valid_from`-first (ties broken by version id) so that, were the store
/// ever to retain multiple co-current versions, the output would remain
/// deterministic.
pub struct TemporalNodeRangeScanIterator {
    results: std::vec::IntoIter<Result<QueryRow>>,
}

impl TemporalNodeRangeScanIterator {
    /// Build a range-scan iterator, reconstructing all overlapping versions up
    /// front under a single read lock.
    ///
    /// # Arguments
    ///
    /// * `node_ids` - Candidate node ids (typically every ever-versioned node)
    /// * `valid_from` - Inclusive start of the valid-time range
    /// * `valid_to` - Exclusive end of the valid-time range
    /// * `transaction_time` - Transaction time the range is observed at
    /// * `historical` - Historical storage
    /// * `label_filter` - Optional label the versions must match
    pub fn new(
        node_ids: Vec<NodeId>,
        valid_from: Timestamp,
        valid_to: Timestamp,
        transaction_time: Timestamp,
        historical: Arc<RwLock<HistoricalStorage>>,
        label_filter: Option<String>,
    ) -> Self {
        // An invalid range (end < start) yields nothing rather than erroring:
        // the converter already validated it, but defend anyway.
        let Ok(range) = crate::core::temporal::TimeRange::new(valid_from, valid_to) else {
            return TemporalNodeRangeScanIterator {
                results: Vec::new().into_iter(),
            };
        };

        // Resolve the label filter to its interned id once. If the label was
        // never interned, nothing can match -- yield an empty result.
        let interned_label = label_filter
            .as_ref()
            .and_then(|label| GLOBAL_INTERNER.get_id(label));
        if label_filter.is_some() && interned_label.is_none() {
            return TemporalNodeRangeScanIterator {
                results: Vec::new().into_iter(),
            };
        }

        let guard = historical.read();
        let mut results: Vec<Result<QueryRow>> = Vec::new();

        for node_id in node_ids {
            // Walk the version chain from the head backwards.
            let Some(head) = guard.get_current_node_version(node_id) else {
                continue;
            };
            let mut selected = Vec::new();
            let mut cursor = Some(head);
            while let Some(vid) = cursor {
                let Some(version) = guard.get_node_version(vid) else {
                    break;
                };
                cursor = version.prev_version;

                // Label filter (label can change across versions).
                if let Some(lbl) = interned_label
                    && version.label != lbl
                {
                    continue;
                }
                // Keep only versions BELIEVED at the observation transaction time
                // -- the exact same tx-visibility predicate the point-in-time
                // `AS OF` selector (`find_node_version_at_time`) uses. This makes
                // the range scan an as-of-TT snapshot ACROSS the valid range:
                // versions whose transaction interval was closed by a later
                // correction or retraction (beliefs no longer held at TT) are
                // excluded, exactly as a point `AS OF` would exclude them. Because
                // at a fixed TT there is at most one version per valid instant,
                // this also yields no duplicate rows.
                if !version
                    .temporal
                    .transaction_time()
                    .contains(transaction_time)
                {
                    continue;
                }
                // Keep versions whose valid interval overlaps the range.
                // (`overlaps` already excludes empty tombstone intervals.)
                if !version.temporal.valid_time().overlaps(&range) {
                    continue;
                }
                selected.push((
                    version.temporal.valid_time().start(),
                    version.node_id,
                    version.label,
                    vid,
                ));
            }

            // Deterministic per-node ordering: oldest valid_from first.
            selected.sort_by(|a, b| a.0.cmp(&b.0).then(a.3.cmp(&b.3)));

            for (valid_from_ts, matched_node_id, matched_label, vid) in selected {
                match guard.reconstruct_node_properties(vid) {
                    Ok(properties) => {
                        let node = Node::new(matched_node_id, matched_label, properties, vid);
                        results
                            .push(Ok(QueryRow::from_entity(EntityResult::Node(node))
                                .at_time(valid_from_ts)));
                    }
                    Err(e) => results.push(Err(e)),
                }
            }
        }

        drop(guard);

        TemporalNodeRangeScanIterator {
            results: results.into_iter(),
        }
    }
}

impl ResultIterator for TemporalNodeRangeScanIterator {
    fn next(&mut self) -> Option<Result<QueryRow>> {
        self.results.next()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.results.size_hint()
    }
}

/// Iterator for graph traversal using BFS.
///
/// # Variable-length depth-range semantics (`*min..max`)
///
/// For a variable-length pattern this iterator binds each **distinct** reachable
/// target node **once**, at its **shortest** hop-distance from the anchor, and
/// emits it iff `min <= shortestDepth <= max`. Because the per-input `visited`
/// set is populated when a node is first enqueued (its shortest BFS depth), a
/// node whose shortest path is shorter than `min` is marked visited early and is
/// **not** re-emitted at a longer, in-range depth; likewise an anchor reachable
/// again only via an in-range cycle is not re-bound.
///
/// This is **node-distinct / shortest-path reachability**, a deliberate v1
/// simplification of openCypher's trail (path-enumeration) semantics. Under full
/// trail semantics `MATCH (a)-[*2..2]->(b)` over `a->x, a->y, x->y` would also
/// bind `y` via `a->x->y`; here `y`'s shortest depth is 1, so it is excluded.
/// Full trail semantics is a tracked follow-up (it requires per-path state and
/// carries cross-lane perf/regression risk in this shared engine).
///
/// # Deduplication Semantics
///
/// The `visited` set is cleared for each new input node. This means:
/// - Each input node gets independent traversal results
/// - If multiple input nodes can reach the same target, it appears multiple times
/// - This is intentional for path-based semantics (e.g., "all friends of each person")
///
/// For global deduplication across all inputs, wrap the output in a `DistinctIterator`.
///
/// # Example
///
/// ```text
/// Input: [A, B]
/// Graph: A → C, B → C
///
/// Output: [C (from A), C (from B)]  // C appears twice
/// ```
pub struct TraversalIterator {
    input: Box<dyn ResultIterator>,
    direction: Direction,
    label: Option<String>,
    /// Minimum depth (inclusive) at which a reached node is emitted. A node is
    /// bound iff `min_depth <= shortestDepth <= depth` (node-distinct /
    /// shortest-path reachability; see the struct-level docs).
    min_depth: usize,
    /// Maximum depth (inclusive); BFS expansion stops beyond this depth.
    depth: usize,
    current: Arc<CurrentStorage>,
    historical: Arc<RwLock<HistoricalStorage>>,
    /// Optional temporal context (valid_time, transaction_time) for edge filtering.
    /// When present, only edges that existed at the specified point in time are traversed.
    temporal_context: Option<(Timestamp, Timestamp)>,
    // BFS state - reset for each input node (see doc comment above)
    frontier: VecDeque<(NodeId, Vec<EntityId>, usize)>,
    visited: HashSet<NodeId>,
    input_exhausted: bool,
}

impl TraversalIterator {
    /// Initialize a Breadth-First Search (BFS) graph traversal iterator.
    ///
    /// # Why?
    /// This is the core engine for `MATCH (a)-[*]->(b)` operations. It manages
    /// a frontier of visited nodes to prevent infinite loops in cyclic graphs,
    /// and conditionally queries the historical storage if a temporal context is present.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        input: Box<dyn ResultIterator>,
        direction: Direction,
        label: Option<String>,
        min_depth: usize,
        depth: usize,
        current: Arc<CurrentStorage>,
        historical: Arc<RwLock<HistoricalStorage>>,
        temporal_context: Option<(Timestamp, Timestamp)>,
    ) -> Self {
        TraversalIterator {
            input,
            direction,
            label,
            min_depth,
            depth,
            current,
            historical,
            temporal_context,
            frontier: VecDeque::new(),
            visited: HashSet::new(),
            input_exhausted: false,
        }
    }

    /// Check if an edge existed at the specified temporal context using a pre-acquired lock guard.
    /// Returns true if no temporal context is set (current state query).
    #[inline]
    fn edge_visible_at_time(
        &self,
        edge_id: crate::core::EdgeId,
        historical_guard: &Option<parking_lot::RwLockReadGuard<'_, HistoricalStorage>>,
    ) -> bool {
        match self.temporal_context {
            Some((valid_time, tx_time)) => {
                // Use the pre-acquired guard to avoid per-edge lock acquisition
                historical_guard
                    .as_ref()
                    .expect("historical_guard must be Some when temporal_context is Some")
                    .find_edge_version_at_time(edge_id, valid_time, tx_time)
                    .is_some()
            }
            None => true, // No temporal context, use current state
        }
    }

    fn get_neighbors(&self, node_id: NodeId) -> Vec<(NodeId, crate::core::EdgeId)> {
        // Acquire historical lock ONCE for all edge checks in this call.
        // This avoids the performance regression of acquiring per-edge locks.
        let historical_guard = self.temporal_context.map(|_| self.historical.read());

        match self.direction {
            Direction::Outgoing => {
                // Use iterator methods to avoid intermediate Vec allocation (Issue #187)
                if let Some(ref label) = self.label {
                    self.current
                        .get_outgoing_edges_with_label_iter(node_id, label)
                        .filter_map(|edge_id| {
                            if !self.edge_visible_at_time(edge_id, &historical_guard) {
                                return None;
                            }
                            // Zero-copy: only get target NodeId, not full Edge (Issue #190)
                            self.current
                                .get_edge_target(edge_id)
                                .ok()
                                .map(|target| (target, edge_id))
                        })
                        .collect()
                } else {
                    self.current
                        .get_outgoing_edges_iter(node_id)
                        .filter_map(|edge_id| {
                            if !self.edge_visible_at_time(edge_id, &historical_guard) {
                                return None;
                            }
                            // Zero-copy: only get target NodeId, not full Edge (Issue #190)
                            self.current
                                .get_edge_target(edge_id)
                                .ok()
                                .map(|target| (target, edge_id))
                        })
                        .collect()
                }
            }
            Direction::Incoming => {
                // Use iterator methods to avoid intermediate Vec allocation (Issue #187)
                if let Some(ref label) = self.label {
                    self.current
                        .get_incoming_edges_with_label_iter(node_id, label)
                        .filter_map(|edge_id| {
                            if !self.edge_visible_at_time(edge_id, &historical_guard) {
                                return None;
                            }
                            // Zero-copy: only get source NodeId, not full Edge (Issue #190)
                            self.current
                                .get_edge_source(edge_id)
                                .ok()
                                .map(|source| (source, edge_id))
                        })
                        .collect()
                } else {
                    self.current
                        .get_incoming_edges_iter(node_id)
                        .filter_map(|edge_id| {
                            if !self.edge_visible_at_time(edge_id, &historical_guard) {
                                return None;
                            }
                            // Zero-copy: only get source NodeId, not full Edge (Issue #190)
                            self.current
                                .get_edge_source(edge_id)
                                .ok()
                                .map(|source| (source, edge_id))
                        })
                        .collect()
                }
            }
            Direction::Both => {
                // Use iterator methods to avoid intermediate Vec allocation (Issue #187)
                // Helper closure to process edges and add to neighbors
                // Zero-copy: only get target NodeId, not full Edge (Issue #190)
                let process_outgoing =
                    |edge_id, neighbors: &mut Vec<(NodeId, crate::core::EdgeId)>| {
                        if !self.edge_visible_at_time(edge_id, &historical_guard) {
                            return;
                        }
                        if let Ok(target) = self.current.get_edge_target(edge_id) {
                            neighbors.push((target, edge_id));
                        }
                    };

                // Zero-copy: only get source NodeId, not full Edge (Issue #190)
                let process_incoming =
                    |edge_id, neighbors: &mut Vec<(NodeId, crate::core::EdgeId)>| {
                        if !self.edge_visible_at_time(edge_id, &historical_guard) {
                            return;
                        }
                        if let Ok(source) = self.current.get_edge_source(edge_id) {
                            neighbors.push((source, edge_id));
                        }
                    };

                if let Some(ref label) = self.label {
                    // ⚡ Bolt Optimization: Instantiate iterators once to avoid duplicate lookups,
                    // calculate required capacity, and pre-allocate to prevent heap reallocations.
                    let out_iter = self
                        .current
                        .get_outgoing_edges_with_label_iter(node_id, label);
                    let in_iter = self
                        .current
                        .get_incoming_edges_with_label_iter(node_id, label);
                    let capacity = out_iter.size_hint().0 + in_iter.size_hint().0;

                    let mut neighbors = Vec::with_capacity(capacity);
                    for edge_id in out_iter {
                        process_outgoing(edge_id, &mut neighbors);
                    }
                    for edge_id in in_iter {
                        process_incoming(edge_id, &mut neighbors);
                    }
                    neighbors
                } else {
                    // ⚡ Bolt Optimization: Instantiate iterators once to avoid duplicate lookups,
                    // calculate required capacity, and pre-allocate to prevent heap reallocations.
                    let out_iter = self.current.get_outgoing_edges_iter(node_id);
                    let in_iter = self.current.get_incoming_edges_iter(node_id);
                    let capacity = out_iter.size_hint().0 + in_iter.size_hint().0;

                    let mut neighbors = Vec::with_capacity(capacity);
                    for edge_id in out_iter {
                        process_outgoing(edge_id, &mut neighbors);
                    }
                    for edge_id in in_iter {
                        process_incoming(edge_id, &mut neighbors);
                    }
                    neighbors
                }
            }
        }
    }
}

impl ResultIterator for TraversalIterator {
    fn next(&mut self) -> Option<Result<QueryRow>> {
        loop {
            // Process current frontier
            if let Some((node_id, path, current_depth)) = self.frontier.pop_front() {
                // Expansion and yielding are INDEPENDENT so a range like `*1..3`
                // both emits intermediate-depth nodes AND keeps exploring to the
                // maximum depth. First expand (if we have not reached the max
                // depth), enqueuing unvisited neighbors one hop deeper.
                if current_depth < self.depth {
                    let neighbors = self.get_neighbors(node_id);
                    for (target, edge_id) in neighbors {
                        // Node-distinct / shortest-path reachability: a node is
                        // enqueued (and thus later emitted) once, at its shortest
                        // BFS depth. This also makes cyclic graphs terminate. It
                        // is a v1 simplification of openCypher trail semantics --
                        // a target whose shortest path is below `min_depth` is
                        // marked visited here and never re-emitted deeper (see the
                        // struct-level docs).
                        if self.visited.insert(target) {
                            // ⚡ Bolt Optimization: Pre-allocate capacity for new path to avoid reallocations.
                            // We are adding exactly 2 elements (edge and node) to the current path length.
                            let mut new_path = Vec::with_capacity(path.len() + 2);
                            new_path.extend_from_slice(&path);
                            new_path.push(EntityId::Edge(edge_id));
                            new_path.push(EntityId::Node(target));
                            self.frontier
                                .push_back((target, new_path, current_depth + 1));
                        }
                    }
                }

                // Then, if this node's depth falls within [min_depth, depth],
                // yield it. The depth-0 start node is never emitted.
                if current_depth >= 1
                    && current_depth >= self.min_depth
                    && current_depth <= self.depth
                {
                    match self.current.get_node(node_id) {
                        Ok(node) => {
                            return Some(Ok(QueryRow::with_path(EntityResult::Node(node), path)));
                        }
                        Err(e) => return Some(Err(e)),
                    }
                }
                continue;
            }

            // Frontier exhausted, get next from input
            if self.input_exhausted {
                return None;
            }

            match self.input.next() {
                Some(Ok(row)) => {
                    if let Some(node_id) = row.entity.node_id() {
                        self.visited.clear();
                        self.visited.insert(node_id);
                        self.frontier
                            .push_back((node_id, vec![EntityId::Node(node_id)], 0));
                    }
                }
                Some(Err(e)) => return Some(Err(e)),
                None => {
                    self.input_exhausted = true;
                    // Process any remaining frontier
                    if self.frontier.is_empty() {
                        return None;
                    }
                }
            }
        }
    }
}

/// Iterator for filtering results.
///
/// # Example
///
/// ```rust
/// use aletheiadb::query::executor::{FilterIterator, NodeScanIterator};
/// use aletheiadb::query::ir::Predicate;
/// use std::sync::Arc;
///
/// let current = Arc::new(aletheiadb::storage::CurrentStorage::new());
/// let input = Box::new(NodeScanIterator::new(Some("Person".to_string()), current));
/// let predicate = Predicate::eq("name", "Alice");
/// let filter_iter = FilterIterator::new(input, predicate);
///
/// // Iterate results
/// // for row in filter_iter { ... }
/// ```
///
/// Iterator that applies a predicate filter.
///
/// Pulls rows from the input and yields only those matching the predicate.
pub struct FilterIterator {
    input: Box<dyn ResultIterator>,
    predicate: Predicate,
}

impl FilterIterator {
    /// Create a new FilterIterator that filters results based on the predicate.
    pub fn new(input: Box<dyn ResultIterator>, predicate: Predicate) -> Self {
        FilterIterator { input, predicate }
    }

    fn evaluate(&self, node: &Node) -> bool {
        self.evaluate_predicate(&self.predicate, node)
    }

    fn evaluate_predicate(&self, predicate: &Predicate, node: &Node) -> bool {
        match predicate {
            Predicate::True => true,
            Predicate::False => false,
            Predicate::Eq { key, value } => self.evaluate_eq(node, key, value),
            Predicate::Ne { key, value } => self.evaluate_ne(node, key, value),
            Predicate::Gt { key, value } => self.evaluate_gt(node, key, value),
            Predicate::Lt { key, value } => self.evaluate_lt(node, key, value),
            Predicate::Gte { key, value } => self.evaluate_gte(node, key, value),
            Predicate::Lte { key, value } => self.evaluate_lte(node, key, value),
            Predicate::Exists(key) => node.properties.get(key).is_some(),
            Predicate::NotExists(key) => node.properties.get(key).is_none(),
            Predicate::Contains { key, substring } => self.evaluate_contains(node, key, substring),
            Predicate::StartsWith { key, prefix } => self.evaluate_starts_with(node, key, prefix),
            Predicate::EndsWith { key, suffix } => self.evaluate_ends_with(node, key, suffix),
            Predicate::In { key, values } => self.evaluate_in(node, key, values),
            Predicate::And(preds) => preds.iter().all(|p| self.evaluate_predicate(p, node)),
            Predicate::Or(preds) => preds.iter().any(|p| self.evaluate_predicate(p, node)),
            Predicate::Not(pred) => !self.evaluate_predicate(pred, node),
        }
    }

    fn evaluate_eq(&self, node: &Node, key: &str, value: &PredicateValue) -> bool {
        let Some(prop) = node.properties.get(key) else {
            return false;
        };
        self.compare_eq(prop, value)
    }

    fn evaluate_ne(&self, node: &Node, key: &str, value: &PredicateValue) -> bool {
        let Some(prop) = node.properties.get(key) else {
            return true; // Non-existent != anything
        };
        !self.compare_eq(prop, value)
    }

    fn evaluate_gt(&self, node: &Node, key: &str, value: &PredicateValue) -> bool {
        let Some(prop) = node.properties.get(key) else {
            return false;
        };
        self.compare_gt(prop, value)
    }

    fn evaluate_lt(&self, node: &Node, key: &str, value: &PredicateValue) -> bool {
        let Some(prop) = node.properties.get(key) else {
            return false;
        };
        self.compare_lt(prop, value)
    }

    fn evaluate_gte(&self, node: &Node, key: &str, value: &PredicateValue) -> bool {
        let Some(prop) = node.properties.get(key) else {
            return false;
        };
        self.compare_gte(prop, value)
    }

    fn evaluate_lte(&self, node: &Node, key: &str, value: &PredicateValue) -> bool {
        let Some(prop) = node.properties.get(key) else {
            return false;
        };
        self.compare_lte(prop, value)
    }

    fn evaluate_contains(&self, node: &Node, key: &str, substring: &str) -> bool {
        let Some(PropertyValue::String(s)) = node.properties.get(key) else {
            return false;
        };
        s.contains(substring)
    }

    fn evaluate_starts_with(&self, node: &Node, key: &str, prefix: &str) -> bool {
        let Some(PropertyValue::String(s)) = node.properties.get(key) else {
            return false;
        };
        s.starts_with(prefix)
    }

    fn evaluate_ends_with(&self, node: &Node, key: &str, suffix: &str) -> bool {
        let Some(PropertyValue::String(s)) = node.properties.get(key) else {
            return false;
        };
        s.ends_with(suffix)
    }

    fn evaluate_in(&self, node: &Node, key: &str, values: &[PredicateValue]) -> bool {
        let Some(prop) = node.properties.get(key) else {
            return false;
        };
        values.iter().any(|v| self.compare_eq(prop, v))
    }

    fn compare_eq(&self, prop: &PropertyValue, value: &PredicateValue) -> bool {
        match (prop, value) {
            (PropertyValue::Bool(a), PredicateValue::Bool(b)) => a == b,
            (PropertyValue::Int(a), PredicateValue::Int(b)) => a == b,
            (PropertyValue::Float(a), PredicateValue::Float(b)) => (a - b).abs() < f64::EPSILON,
            (PropertyValue::String(a), PredicateValue::String(b)) => a.as_ref() == b.as_str(),
            (PropertyValue::Null, PredicateValue::Null) => true,
            _ => false,
        }
    }

    fn compare_gt(&self, prop: &PropertyValue, value: &PredicateValue) -> bool {
        match (prop, value) {
            (PropertyValue::Int(a), PredicateValue::Int(b)) => a > b,
            (PropertyValue::Float(a), PredicateValue::Float(b)) => a > b,
            _ => false,
        }
    }

    fn compare_lt(&self, prop: &PropertyValue, value: &PredicateValue) -> bool {
        match (prop, value) {
            (PropertyValue::Int(a), PredicateValue::Int(b)) => a < b,
            (PropertyValue::Float(a), PredicateValue::Float(b)) => a < b,
            _ => false,
        }
    }

    fn compare_gte(&self, prop: &PropertyValue, value: &PredicateValue) -> bool {
        match (prop, value) {
            (PropertyValue::Int(a), PredicateValue::Int(b)) => a >= b,
            (PropertyValue::Float(a), PredicateValue::Float(b)) => a >= b,
            _ => false,
        }
    }

    fn compare_lte(&self, prop: &PropertyValue, value: &PredicateValue) -> bool {
        match (prop, value) {
            (PropertyValue::Int(a), PredicateValue::Int(b)) => a <= b,
            (PropertyValue::Float(a), PredicateValue::Float(b)) => a <= b,
            _ => false,
        }
    }
}

impl FilterIterator {
    /// Evaluate a predicate against a null binding (an unmatched
    /// `OPTIONAL MATCH` row).
    ///
    /// Approximates openCypher three-valued logic at the "not true" level:
    /// any comparison, string predicate, or membership test involving the
    /// null binding is not-true (row filtered), `IS NULL` (encoded as
    /// `Eq { value: Null }`) is true, and `IS NOT NULL` (encoded as
    /// `Ne { value: Null }`) is false.
    ///
    /// Known deviation: `NOT` uses two-valued negation (`NOT (null.p = 1)`
    /// keeps the row where openCypher's `NOT null` would drop it). This
    /// mirrors the engine's existing missing-property semantics.
    fn evaluate_null(&self, predicate: &Predicate) -> bool {
        match predicate {
            Predicate::True => true,
            Predicate::False => false,
            // `x IS NULL` is converted to Eq { value: Null }: true for a null row.
            Predicate::Eq {
                value: PredicateValue::Null,
                ..
            } => true,
            // `x IS NOT NULL` is converted to Ne { value: Null }: false for a null row.
            Predicate::Ne {
                value: PredicateValue::Null,
                ..
            } => false,
            // A null binding has no properties.
            Predicate::NotExists(_) => true,
            Predicate::Exists(_) => false,
            // Any comparison/membership/string predicate against null is not-true.
            Predicate::Eq { .. }
            | Predicate::Ne { .. }
            | Predicate::Gt { .. }
            | Predicate::Gte { .. }
            | Predicate::Lt { .. }
            | Predicate::Lte { .. }
            | Predicate::In { .. }
            | Predicate::Contains { .. }
            | Predicate::StartsWith { .. }
            | Predicate::EndsWith { .. } => false,
            Predicate::And(preds) => preds.iter().all(|p| self.evaluate_null(p)),
            Predicate::Or(preds) => preds.iter().any(|p| self.evaluate_null(p)),
            Predicate::Not(pred) => !self.evaluate_null(pred),
        }
    }
}

impl ResultIterator for FilterIterator {
    fn next(&mut self) -> Option<Result<QueryRow>> {
        loop {
            match self.input.next() {
                Some(Ok(row)) => {
                    if let Some(node) = row.entity.as_node() {
                        if self.evaluate(node) {
                            return Some(Ok(row));
                        }
                        // Filter didn't pass, continue to next
                    } else if row.entity.is_null() {
                        // Null bindings from unmatched OPTIONAL MATCH rows are
                        // evaluated with null semantics (comparisons are
                        // not-true, IS NULL is true).
                        if self.evaluate_null(&self.predicate) {
                            return Some(Ok(row));
                        }
                        // Filter didn't pass, continue to next
                    } else {
                        // Non-node entities pass through
                        return Some(Ok(row));
                    }
                }
                Some(Err(e)) => return Some(Err(e)),
                None => return None,
            }
        }
    }
}

/// Iterator that yields a single, pre-built row.
///
/// Used by [`OptionalApplyIterator`] to seed the per-row optional
/// sub-pipeline with the current input row.
struct SeedRowIterator {
    row: Option<QueryRow>,
}

impl SeedRowIterator {
    fn new(row: QueryRow) -> Self {
        SeedRowIterator { row: Some(row) }
    }
}

impl ResultIterator for SeedRowIterator {
    fn next(&mut self) -> Option<Result<QueryRow>> {
        self.row.take().map(Ok)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let n = usize::from(self.row.is_some());
        (n, Some(n))
    }
}

/// Iterator implementing left-outer (`OPTIONAL MATCH`) semantics.
///
/// For each input row, the configured sub-pipeline (traversal hops and
/// filters) is executed seeded from that row:
///
/// - If it produces at least one row, those rows are yielded (matched case).
/// - If it produces nothing, a single row with [`EntityResult::Null`] is
///   yielded instead, preserving the input row (unmatched case).
///
/// Because the sub-pipeline includes the optional pattern's inline property
/// filters and its `WHERE` clause, filtering happens *before* the
/// matched/unmatched decision, per openCypher semantics.
///
/// When the first step is a `Scan` the iterator is *standalone* (a leading
/// `OPTIONAL MATCH` with no prior rows): the sub-pipeline runs exactly once
/// from its own node scan, and an empty result yields one null row.
///
/// A null seed row (from a preceding unmatched optional) traverses to nothing,
/// so a chained `OPTIONAL MATCH` over it yields another null row -- null
/// propagates without dropping the row.
pub struct OptionalApplyIterator {
    input: Box<dyn ResultIterator>,
    steps: Vec<crate::query::planner::physical::OptionalPhysicalStep>,
    current: Arc<CurrentStorage>,
    historical: Arc<RwLock<HistoricalStorage>>,
    /// True when the first step is a Scan (leading OPTIONAL MATCH form).
    standalone: bool,
    /// The sub-pipeline currently being drained (one per seed row).
    inner: Option<Box<dyn ResultIterator>>,
    /// Whether the current sub-pipeline has produced at least one row.
    inner_matched: bool,
    /// Set when the input (or the single standalone run) is exhausted.
    done: bool,
    /// The seed row currently being processed, kept so an unmatched fallback
    /// preserves its metadata (score, path, timestamp). `None` in the
    /// standalone form, which has no seed row.
    current_seed: Option<QueryRow>,
}

impl OptionalApplyIterator {
    /// Create a new OptionalApplyIterator.
    pub fn new(
        input: Box<dyn ResultIterator>,
        steps: Vec<crate::query::planner::physical::OptionalPhysicalStep>,
        current: Arc<CurrentStorage>,
        historical: Arc<RwLock<HistoricalStorage>>,
    ) -> Self {
        use crate::query::planner::physical::OptionalPhysicalStep;
        let standalone = matches!(steps.first(), Some(OptionalPhysicalStep::Scan { .. }));
        Self {
            input,
            steps,
            current,
            historical,
            standalone,
            inner: None,
            inner_matched: false,
            done: false,
            current_seed: None,
        }
    }

    /// Build the optional sub-pipeline for one seed row (or, for the
    /// standalone form, from the leading scan step).
    fn build_pipeline(&self, seed: Option<QueryRow>) -> Box<dyn ResultIterator> {
        use crate::query::planner::physical::OptionalPhysicalStep;

        let mut iter: Box<dyn ResultIterator> = match seed {
            Some(row) => Box::new(SeedRowIterator::new(row)),
            None => Box::new(EmptyIterator),
        };

        for step in &self.steps {
            iter = match step {
                OptionalPhysicalStep::Scan { label } => {
                    // Source step (standalone form): replaces the seed input.
                    Box::new(NodeScanIterator::new(
                        label.clone(),
                        Arc::clone(&self.current),
                    ))
                }
                OptionalPhysicalStep::Traverse {
                    direction,
                    label,
                    min_depth,
                    depth,
                    temporal_context,
                } => Box::new(TraversalIterator::new(
                    iter,
                    *direction,
                    label.clone(),
                    *min_depth,
                    *depth,
                    Arc::clone(&self.current),
                    Arc::clone(&self.historical),
                    *temporal_context,
                )),
                OptionalPhysicalStep::Filter(predicate) => {
                    Box::new(FilterIterator::new(iter, predicate.clone()))
                }
            };
        }

        iter
    }
}

impl ResultIterator for OptionalApplyIterator {
    fn next(&mut self) -> Option<Result<QueryRow>> {
        loop {
            // Drain the current sub-pipeline, if any.
            if let Some(inner) = self.inner.as_mut() {
                match inner.next() {
                    Some(Ok(row)) => {
                        self.inner_matched = true;
                        return Some(Ok(row));
                    }
                    Some(Err(e)) => {
                        // Abandon the errored seed entirely: a consumer that
                        // iterates past the error must not receive a
                        // fabricated null row for it.
                        self.inner = None;
                        self.inner_matched = false;
                        self.current_seed = None;
                        return Some(Err(e));
                    }
                    None => {
                        let unmatched = !self.inner_matched;
                        self.inner = None;
                        let seed = self.current_seed.take();
                        if unmatched {
                            // Left-outer semantics: preserve the input row --
                            // including its metadata (score, path, timestamp)
                            // -- with a null binding. The standalone form has
                            // no seed row, so it falls back to a bare null row.
                            let mut row =
                                seed.unwrap_or_else(|| QueryRow::from_entity(EntityResult::Null));
                            row.entity = EntityResult::Null;
                            return Some(Ok(row));
                        }
                        continue;
                    }
                }
            }

            if self.done {
                return None;
            }

            if self.standalone {
                // Leading OPTIONAL MATCH: run the scan pipeline exactly once.
                self.inner = Some(self.build_pipeline(None));
                self.inner_matched = false;
                self.done = true;
                continue;
            }

            // Pull the next seed row from the input.
            match self.input.next() {
                Some(Ok(seed)) => {
                    self.current_seed = Some(seed.clone());
                    self.inner = Some(self.build_pipeline(Some(seed)));
                    self.inner_matched = false;
                }
                Some(Err(e)) => return Some(Err(e)),
                None => {
                    self.done = true;
                    return None;
                }
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        // At least one row per remaining input row (matched rows may add
        // more, so no useful upper bound).
        let (lower, _) = self.input.size_hint();
        (lower, None)
    }
}

/// Helper struct for maintaining query rows with similarity scores in a heap.
/// Ordered by score (higher is better) via Ord implementation.
#[derive(Clone)]
struct ScoredRow {
    row: QueryRow,
    score: f32,
}

impl PartialEq for ScoredRow {
    fn eq(&self, other: &Self) -> bool {
        self.score.to_bits() == other.score.to_bits()
    }
}
impl Eq for ScoredRow {}

impl PartialOrd for ScoredRow {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ScoredRow {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Invariant: compute_similarity() filters out non-finite values,
        // so all scores in the heap are finite.
        self.score
            .partial_cmp(&other.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    }
}

/// Iterator for vector reranking.
pub struct VectorRerankIterator {
    sorted: Option<std::vec::IntoIter<Reverse<ScoredRow>>>,
    input: Option<Box<dyn ResultIterator>>,
    embedding: Arc<[f32]>,
    k: usize,
    _current: Arc<CurrentStorage>,
    /// Vector property name, or None if no vector index is configured
    vector_property: Option<String>,
}

impl VectorRerankIterator {
    /// Create a new VectorRerankIterator.
    ///
    /// # Arguments
    /// * `input` - The input iterator to rerank
    /// * `embedding` - The target embedding for similarity comparison
    /// * `k` - Maximum number of results to keep
    /// * `current` - Reference to current storage
    /// * `property_key` - Optional property to use for reranking. If None, uses default.
    pub fn new(
        input: Box<dyn ResultIterator>,
        embedding: Arc<[f32]>,
        k: usize,
        current: Arc<CurrentStorage>,
        property_key: Option<String>,
    ) -> Self {
        // Use explicit property if provided, otherwise get default from storage
        let vector_property = property_key.or_else(|| current.get_vector_property_name());

        VectorRerankIterator {
            sorted: None,
            input: Some(input),
            embedding,
            k,
            _current: current,
            vector_property,
        }
    }

    /// Compute similarity score for a query row if it has a vector property.
    /// Returns None if the node has no vector, or if the similarity is invalid (NaN/Inf).
    fn compute_similarity(&self, row: &QueryRow, vector_property: &str) -> Option<f32> {
        let node = row.entity.as_node()?;
        let PropertyValue::Vector(vec) = node.properties.get(vector_property)? else {
            return None;
        };
        let similarity = cosine_similarity(&self.embedding, vec).ok()?;
        // Reject NaN/Inf values - these indicate invalid input (e.g., zero-length vectors)
        if similarity.is_finite() {
            Some(similarity)
        } else {
            #[cfg(feature = "observability")]
            tracing::debug!(
                "Skipping node {:?} with non-finite similarity score: {}",
                node.id,
                similarity
            );
            None
        }
    }
}

impl ResultIterator for VectorRerankIterator {
    fn next(&mut self) -> Option<Result<QueryRow>> {
        // Lazy initialization: collect and sort on first call
        if self.sorted.is_none() && self.input.is_some() {
            // Check if vector index is configured
            let vector_property = match &self.vector_property {
                Some(prop) => prop.as_str(),
                None => {
                    return Some(Err(crate::core::error::Error::Vector(
                        crate::core::error::VectorError::IndexError(
                            "VectorRerank requires a vector index to be enabled. \
                             Call db.vector_index(\"...\").hnsw(...).enable() first."
                                .to_string(),
                        ),
                    )));
                }
            };

            let mut input = self.input.take()?;
            // Use a min-heap to keep the top-k results
            let mut heap = BinaryHeap::with_capacity(self.k);

            while let Some(result) = input.next() {
                match result {
                    Ok(row) => {
                        // Get vector from node and compute similarity
                        if let Some(similarity) = self.compute_similarity(&row, vector_property) {
                            debug_assert!(similarity.is_finite(), "Non-finite similarity score");
                            if heap.len() < self.k {
                                heap.push(Reverse(ScoredRow {
                                    row,
                                    score: similarity,
                                }));
                            } else {
                                #[allow(clippy::collapsible_if)]
                                if let Some(Reverse(min_row)) = heap.peek() {
                                    if similarity > min_row.score {
                                        heap.pop();
                                        heap.push(Reverse(ScoredRow {
                                            row,
                                            score: similarity,
                                        }));
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => return Some(Err(e)),
                }
            }

            // Convert heap to sorted vector (descending score)
            // BinaryHeap::into_sorted_vec() returns elements in ascending order of T.
            // Since T is Reverse<ScoredRow>, the order is:
            // [Smallest Reverse<ScoredRow>, ..., Largest Reverse<ScoredRow>]
            // Smallest Reverse<ScoredRow> corresponds to Largest ScoredRow (highest score).
            // So the result is [Highest Score, ..., Lowest Score], which is exactly what we want.
            self.sorted = Some(heap.into_sorted_vec().into_iter());
        }

        self.sorted.as_mut()?.next().map(|Reverse(item)| {
            let mut row = item.row;
            row.score = Some(item.score);
            Ok(row)
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        if let Some(ref sorted) = self.sorted {
            sorted.size_hint()
        } else {
            (0, Some(self.k))
        }
    }
}

/// Iterator for limiting results.
///
/// # Example
///
/// ```rust
/// use aletheiadb::query::executor::{LimitIterator, NodeScanIterator};
/// use std::sync::Arc;
///
/// let current = Arc::new(aletheiadb::storage::CurrentStorage::new());
/// let input = Box::new(NodeScanIterator::new(Some("Person".to_string()), current));
///
/// // Skip 5, take 10
/// let limit_iter = LimitIterator::new(input, 5, 10);
/// ```
pub struct LimitIterator {
    input: Box<dyn ResultIterator>,
    offset: usize,
    count: usize,
    skipped: usize,
    returned: usize,
}

impl LimitIterator {
    /// Create a new LimitIterator that applies offset and limit to the input.
    pub fn new(input: Box<dyn ResultIterator>, offset: usize, count: usize) -> Self {
        LimitIterator {
            input,
            offset,
            count,
            skipped: 0,
            returned: 0,
        }
    }
}

impl ResultIterator for LimitIterator {
    fn next(&mut self) -> Option<Result<QueryRow>> {
        // Skip offset
        while self.skipped < self.offset {
            match self.input.next() {
                Some(Ok(_)) => self.skipped += 1,
                Some(Err(e)) => return Some(Err(e)),
                None => return None,
            }
        }

        // Check count limit
        if self.returned >= self.count {
            return None;
        }

        match self.input.next() {
            Some(result) => {
                self.returned += 1;
                Some(result)
            }
            None => None,
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.count.saturating_sub(self.returned);
        let (lower, upper) = self.input.size_hint();
        (lower.min(remaining), upper.map(|u| u.min(remaining)))
    }
}

/// Wrapper iterator that strips provenance metadata when include_provenance is false.
///
/// This iterator conditionally removes timestamp and path information from QueryRow
/// results based on the query hint. When include_provenance is false, these fields
/// are set to None for better performance and reduced memory usage.
pub struct ProvenanceFilterIterator {
    inner: Box<dyn ResultIterator>,
    include_provenance: bool,
}

impl ProvenanceFilterIterator {
    /// Create a new ProvenanceFilterIterator that conditionally strips metadata.
    pub fn new(inner: Box<dyn ResultIterator>, include_provenance: bool) -> Self {
        ProvenanceFilterIterator {
            inner,
            include_provenance,
        }
    }
}

impl ResultIterator for ProvenanceFilterIterator {
    fn next(&mut self) -> Option<Result<QueryRow>> {
        self.inner.next().map(|result| {
            result.map(|mut row| {
                if !self.include_provenance {
                    // Strip provenance metadata
                    row.path = None;
                    row.timestamp = None;
                }
                row
            })
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

/// Iterator for projecting specific properties from query results.
pub struct ProjectIterator {
    input: Box<dyn ResultIterator>,
    properties: Vec<String>,
}

impl ProjectIterator {
    /// Create a new ProjectIterator that projects specific properties from the results.
    pub fn new(input: Box<dyn ResultIterator>, mut properties: Vec<String>) -> Self {
        // Deduplicate properties to prevent errors when projecting same property multiple times
        properties.sort();
        properties.dedup();
        ProjectIterator { input, properties }
    }
}

impl ResultIterator for ProjectIterator {
    fn next(&mut self) -> Option<Result<QueryRow>> {
        match self.input.next() {
            Some(Ok(mut row)) => {
                if let Some(node) = row.entity.as_node() {
                    let mut new_props = crate::core::PropertyMapBuilder::new();
                    for prop in &self.properties {
                        if let Some(val) = node.properties.get(prop) {
                            new_props = match new_props.try_insert(prop, val.clone()) {
                                Ok(p) => p,
                                Err(e) => return Some(Err(e)),
                            };
                        }
                    }
                    let new_node = crate::core::graph::Node::new(
                        node.id,
                        node.label,
                        new_props.build(),
                        node.current_version,
                    );
                    row.entity = EntityResult::Node(new_node);
                }
                Some(Ok(row))
            }
            other => other,
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.input.size_hint()
    }
}

/// Convert a `PredicateValue` to a `PropertyValue` for storage-level lookups.
fn predicate_to_property_value(pv: &PredicateValue) -> PropertyValue {
    match pv {
        PredicateValue::Null => PropertyValue::Null,
        PredicateValue::Bool(b) => PropertyValue::Bool(*b),
        PredicateValue::Int(i) => PropertyValue::Int(*i),
        PredicateValue::Float(f) => PropertyValue::Float(*f),
        PredicateValue::String(s) => PropertyValue::String(Arc::from(s.as_str())),
    }
}

/// Iterator for property-based node scans.
///
/// Calls `CurrentStorage::find_nodes_by_property` to get matching node IDs,
/// then resolves each to a full `Node` for the query result.
pub struct PropertyScanIterator {
    current: Arc<CurrentStorage>,
    initialized: bool,
    node_ids: Option<std::vec::IntoIter<NodeId>>,
    label: String,
    property_value: PropertyValue,
    property_key: String,
}

impl PropertyScanIterator {
    /// Initialize a full-scan iterator that evaluates a property predicate against all nodes.
    ///
    /// # Why?
    /// Use this as a fallback when no index is available for the requested `label` and `key`.
    /// It eagerly loads all matching nodes into memory, so it is best used on small datasets.
    pub fn new(
        label: String,
        key: String,
        value: &PredicateValue,
        current: Arc<CurrentStorage>,
    ) -> Self {
        PropertyScanIterator {
            current,
            initialized: false,
            node_ids: None,
            label,
            property_value: predicate_to_property_value(value),
            property_key: key,
        }
    }

    fn initialize(&mut self) {
        if self.initialized {
            return;
        }
        self.initialized = true;
        let ids = self.current.find_nodes_by_property(
            &self.label,
            &self.property_key,
            &self.property_value,
        );
        self.node_ids = Some(ids.into_iter());
    }
}

impl ResultIterator for PropertyScanIterator {
    fn next(&mut self) -> Option<Result<QueryRow>> {
        self.initialize();

        match self.node_ids.as_mut()?.next() {
            Some(id) => match self.current.get_node(id) {
                Ok(node) => Some(Ok(QueryRow::from_entity(EntityResult::Node(node)))),
                Err(e) => Some(Err(e)),
            },
            None => None,
        }
    }
}

// ============================================================================
// Aggregation, DISTINCT, and ORDER BY iterators
// ============================================================================

/// Read a node property value from a row, for grouping / aggregation / sorting.
///
/// Returns `None` when the row has no backing node (e.g. a null binding or a
/// computed aggregate row) or the property is absent.
fn row_property<'a>(row: &'a QueryRow, key: &str) -> Option<&'a PropertyValue> {
    row.entity.as_node().and_then(|n| n.properties.get(key))
}

/// Numeric view of a scalar property (`Int`/`Float`), used for sum/avg and
/// numeric ordering. `None` for non-numeric values.
fn property_as_f64(value: &PropertyValue) -> Option<f64> {
    match value {
        PropertyValue::Int(i) => Some(*i as f64),
        PropertyValue::Float(f) => Some(*f),
        _ => None,
    }
}

/// Total order over comparable scalar property values (numbers compared
/// numerically, strings lexicographically, bools by value). Non-comparable or
/// mixed pairs compare `Equal` so they retain input order.
fn compare_property(a: &PropertyValue, b: &PropertyValue) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (PropertyValue::Int(x), PropertyValue::Int(y)) => x.cmp(y),
        (
            PropertyValue::Int(_) | PropertyValue::Float(_),
            PropertyValue::Int(_) | PropertyValue::Float(_),
        ) => match (property_as_f64(a), property_as_f64(b)) {
            (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(Ordering::Equal),
            _ => Ordering::Equal,
        },
        (PropertyValue::String(x), PropertyValue::String(y)) => x.as_ref().cmp(y.as_ref()),
        (PropertyValue::Bool(x), PropertyValue::Bool(y)) => x.cmp(y),
        _ => Ordering::Equal,
    }
}

/// A hashable, `Eq` projection of a scalar property value used both for
/// grouping keys and for `DISTINCT` deduplication of aggregate arguments.
/// Floats are keyed by their bit pattern (total order); non-scalar values fall
/// back to their debug representation.
#[derive(Clone, PartialEq, Eq, Hash)]
enum GroupKeyValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(u64),
    Str(String),
    Other(String),
}

impl GroupKeyValue {
    fn from_property(value: Option<&PropertyValue>) -> Self {
        match value {
            None | Some(PropertyValue::Null) => GroupKeyValue::Null,
            Some(PropertyValue::Bool(b)) => GroupKeyValue::Bool(*b),
            Some(PropertyValue::Int(i)) => GroupKeyValue::Int(*i),
            Some(PropertyValue::Float(f)) => Self::float_key(*f),
            Some(PropertyValue::String(s)) => GroupKeyValue::Str(s.to_string()),
            Some(other) => GroupKeyValue::Other(format!("{other:?}")),
        }
    }

    /// Map a float to a grouping/DISTINCT key, canonicalizing `-0.0 -> 0.0`
    /// (signed zeros group together) and unifying an integral float with the
    /// equal integer (so `1` and `1.0` land in the same group / distinct set).
    fn float_key(f: f64) -> Self {
        let f = if f == 0.0 { 0.0 } else { f };
        if f.is_finite() && f.fract() == 0.0 && f >= i64::MIN as f64 && f <= i64::MAX as f64 {
            GroupKeyValue::Int(f as i64)
        } else {
            GroupKeyValue::Float(f.to_bits())
        }
    }
}

/// The resolved per-row input to one aggregate, computed once per row in
/// [`AggregateIterator::drain`].
enum AggInput<'a> {
    /// `count(*)` -- count the row unconditionally.
    Star,
    /// `count(n)` -- count the row iff its bound entity is non-null.
    Entity { present: bool },
    /// `func(n.prop)` -- the row's value for the property (`None`/`Null` =
    /// absent, skipped by every value aggregate).
    Value(Option<&'a PropertyValue>),
}

/// Per-group, per-aggregate accumulator.
struct AggAccumulator {
    func: AggregateFunc,
    distinct: bool,
    /// Values already counted, for the `DISTINCT` quantifier.
    seen: HashSet<GroupKeyValue>,
    count: i64,
    /// Integer running sum (i128 headroom against overflow) for all-integer sums.
    sum_i: i128,
    /// Floating running sum, always maintained (used for avg and float sums).
    sum_f: f64,
    saw_float: bool,
    numeric_count: i64,
    min: Option<PropertyValue>,
    max: Option<PropertyValue>,
    collected: Vec<PropertyValue>,
}

impl AggAccumulator {
    fn new(spec: &AggregateSpec) -> Self {
        AggAccumulator {
            func: spec.func,
            distinct: spec.distinct,
            seen: HashSet::new(),
            count: 0,
            sum_i: 0,
            sum_f: 0.0,
            saw_float: false,
            numeric_count: 0,
            min: None,
            max: None,
            collected: Vec::new(),
        }
    }

    /// Fold one input row into the accumulator.
    ///
    /// `count(*)` ([`AggInput::Star`]) counts the row unconditionally and
    /// `count(n)` ([`AggInput::Entity`]) counts non-null bindings, both
    /// ignoring `DISTINCT`. Otherwise the value ([`AggInput::Value`]) feeds the
    /// function; `None`/`Null` values are skipped (openCypher semantics).
    fn update(&mut self, input: AggInput<'_>) {
        let v = match input {
            AggInput::Star => {
                // Only Count uses Star (enforced by the converter).
                self.count += 1;
                return;
            }
            AggInput::Entity { present } => {
                // Only Count uses Entity; count non-null bindings.
                if present {
                    self.count += 1;
                }
                return;
            }
            AggInput::Value(Some(v)) if !matches!(v, PropertyValue::Null) => v,
            AggInput::Value(_) => return,
        };

        if self.distinct {
            let key = GroupKeyValue::from_property(Some(v));
            if !self.seen.insert(key) {
                return;
            }
        }

        match self.func {
            AggregateFunc::Count => self.count += 1,
            AggregateFunc::Sum | AggregateFunc::Avg => {
                if let Some(f) = property_as_f64(v) {
                    self.sum_f += f;
                    self.numeric_count += 1;
                    match v {
                        PropertyValue::Int(i) => self.sum_i += i128::from(*i),
                        PropertyValue::Float(_) => self.saw_float = true,
                        _ => {}
                    }
                }
            }
            AggregateFunc::Min => {
                let replace = match &self.min {
                    None => true,
                    Some(m) => compare_property(v, m) == std::cmp::Ordering::Less,
                };
                if replace {
                    self.min = Some(v.clone());
                }
            }
            AggregateFunc::Max => {
                let replace = match &self.max {
                    None => true,
                    Some(m) => compare_property(v, m) == std::cmp::Ordering::Greater,
                };
                if replace {
                    self.max = Some(v.clone());
                }
            }
            AggregateFunc::Collect => self.collected.push(v.clone()),
        }
    }

    /// Produce the final aggregate value for this group.
    fn finalize(self) -> PropertyValue {
        match self.func {
            AggregateFunc::Count => PropertyValue::Int(self.count),
            AggregateFunc::Sum => {
                if self.numeric_count == 0 {
                    // openCypher: sum over no values is 0 (integer).
                    PropertyValue::Int(0)
                } else if self.saw_float {
                    PropertyValue::Float(self.sum_f)
                } else {
                    // Checked cast: an all-integer sum that overflows i64 falls
                    // back to Float rather than silently wrapping.
                    match i64::try_from(self.sum_i) {
                        Ok(v) => PropertyValue::Int(v),
                        Err(_) => PropertyValue::Float(self.sum_i as f64),
                    }
                }
            }
            AggregateFunc::Avg => {
                if self.numeric_count == 0 {
                    PropertyValue::Null
                } else {
                    PropertyValue::Float(self.sum_f / self.numeric_count as f64)
                }
            }
            AggregateFunc::Min => self.min.unwrap_or(PropertyValue::Null),
            AggregateFunc::Max => self.max.unwrap_or(PropertyValue::Null),
            AggregateFunc::Collect => PropertyValue::Array(Arc::new(self.collected)),
        }
    }
}

/// A single group's state during aggregation.
struct AggGroup {
    /// The group-key values (in `group_keys` order), captured from the first
    /// row of the group for output.
    key_values: Vec<PropertyValue>,
    accumulators: Vec<AggAccumulator>,
}

/// Grouped aggregation iterator (openCypher implicit grouping).
///
/// Eagerly drains its input on the first `next()`, hash-groups rows by the
/// group-key tuple, folds each aggregate per group, and then emits exactly one
/// computed-column [`QueryRow`] per group (via [`QueryRow::from_columns`]) in
/// group-discovery order. A global aggregation (no group keys) always emits one
/// row even over empty input; a grouped aggregation over empty input emits no
/// rows.
pub struct AggregateIterator {
    input: Option<Box<dyn ResultIterator>>,
    group_keys: Vec<AggregateGroupKey>,
    aggregates: Vec<AggregateSpec>,
    output: std::vec::IntoIter<QueryRow>,
    drained: bool,
}

impl AggregateIterator {
    /// Create a new aggregation iterator.
    pub fn new(
        input: Box<dyn ResultIterator>,
        group_keys: Vec<AggregateGroupKey>,
        aggregates: Vec<AggregateSpec>,
    ) -> Self {
        AggregateIterator {
            input: Some(input),
            group_keys,
            aggregates,
            output: Vec::new().into_iter(),
            drained: false,
        }
    }

    fn drain(&mut self) -> Result<()> {
        let mut input = match self.input.take() {
            Some(i) => i,
            None => return Ok(()),
        };

        let mut order: Vec<Vec<GroupKeyValue>> = Vec::new();
        let mut groups: std::collections::HashMap<Vec<GroupKeyValue>, AggGroup> =
            std::collections::HashMap::new();

        // A global aggregation always yields exactly one row, even over empty
        // input: seed the single (empty-key) group up front.
        if self.group_keys.is_empty() {
            let key: Vec<GroupKeyValue> = Vec::new();
            order.push(key.clone());
            groups.insert(
                key,
                AggGroup {
                    key_values: Vec::new(),
                    accumulators: self.aggregates.iter().map(AggAccumulator::new).collect(),
                },
            );
        }

        while let Some(row) = input.next() {
            let row = row?;
            let key: Vec<GroupKeyValue> = self
                .group_keys
                .iter()
                .map(|gk| GroupKeyValue::from_property(row_property(&row, &gk.property_key)))
                .collect();

            if !groups.contains_key(&key) {
                order.push(key.clone());
                let key_values = self
                    .group_keys
                    .iter()
                    .map(|gk| {
                        row_property(&row, &gk.property_key)
                            .cloned()
                            .unwrap_or(PropertyValue::Null)
                    })
                    .collect();
                let accumulators = self.aggregates.iter().map(AggAccumulator::new).collect();
                groups.insert(
                    key.clone(),
                    AggGroup {
                        key_values,
                        accumulators,
                    },
                );
            }

            let group = groups
                .get_mut(&key)
                .expect("group inserted above must be present");
            for (spec, acc) in self.aggregates.iter().zip(group.accumulators.iter_mut()) {
                let input = match &spec.arg {
                    AggregateArg::Star => AggInput::Star,
                    AggregateArg::Entity => AggInput::Entity {
                        present: !row.entity.is_null(),
                    },
                    AggregateArg::Property(k) => AggInput::Value(row_property(&row, k)),
                };
                acc.update(input);
            }
        }

        let mut rows = Vec::with_capacity(order.len());
        for key in order {
            let group = groups.remove(&key).expect("group present in order");
            let mut columns: Vec<(String, PropertyValue)> =
                Vec::with_capacity(self.group_keys.len() + self.aggregates.len());
            for (gk, val) in self.group_keys.iter().zip(group.key_values) {
                columns.push((gk.alias.clone(), val));
            }
            for (spec, acc) in self.aggregates.iter().zip(group.accumulators) {
                columns.push((spec.alias.clone(), acc.finalize()));
            }
            rows.push(QueryRow::from_columns(columns));
        }
        self.output = rows.into_iter();
        Ok(())
    }
}

impl ResultIterator for AggregateIterator {
    fn next(&mut self) -> Option<Result<QueryRow>> {
        if !self.drained {
            self.drained = true;
            if let Err(e) = self.drain() {
                return Some(Err(e));
            }
        }
        self.output.next().map(Ok)
    }
}

/// A whole-row deduplication key for `DISTINCT`.
#[derive(PartialEq, Eq, Hash)]
enum DistinctRowKey {
    /// Keyed by the row's entity identity (node/edge id, or `None` for a null
    /// binding).
    Entity(Option<EntityId>),
    /// A computed aggregate row, keyed by its rendered columns.
    Columns(String),
}

/// `RETURN DISTINCT` iterator: yields each distinct row once, preserving first
/// occurrence order.
pub struct DistinctIterator {
    input: Box<dyn ResultIterator>,
    seen: HashSet<DistinctRowKey>,
}

impl DistinctIterator {
    /// Create a new DISTINCT iterator over `input`.
    pub fn new(input: Box<dyn ResultIterator>) -> Self {
        DistinctIterator {
            input,
            seen: HashSet::new(),
        }
    }

    fn key(row: &QueryRow) -> DistinctRowKey {
        if let Some(cols) = &row.columns {
            DistinctRowKey::Columns(format!("{cols:?}"))
        } else {
            DistinctRowKey::Entity(row.entity.id())
        }
    }
}

impl ResultIterator for DistinctIterator {
    fn next(&mut self) -> Option<Result<QueryRow>> {
        loop {
            match self.input.next()? {
                Ok(row) => {
                    if self.seen.insert(Self::key(&row)) {
                        return Some(Ok(row));
                    }
                }
                Err(e) => return Some(Err(e)),
            }
        }
    }
}

/// Look up a value for ordering by column/property name: first a node property
/// (entity rows), then a computed aggregate column (`row.columns`) so ORDER BY
/// can sort grouped/aggregate rows by a group key or aggregate alias.
fn row_value<'a>(row: &'a QueryRow, key: &str) -> Option<&'a PropertyValue> {
    if let Some(v) = row_property(row, key) {
        return Some(v);
    }
    row.columns
        .as_ref()
        .and_then(|cols| cols.iter().find(|(k, _)| k == key).map(|(_, v)| v))
}

/// `ORDER BY` iterator: buffers the entire input and stably sorts it by one or
/// more keys in precedence order (first key primary), then streams the result.
///
/// Each key carries its own ascending/descending flag. Null placement follows
/// openCypher: a missing/null sort value orders **last** for an ascending key
/// and **first** for a descending key. Property keys fall back to computed
/// aggregate columns via [`row_value`], so grouped results can be ordered by a
/// group key or aggregate alias.
pub struct SortIterator {
    input: Option<Box<dyn ResultIterator>>,
    keys: Vec<(SortKey, bool)>,
    output: std::vec::IntoIter<QueryRow>,
    drained: bool,
}

impl SortIterator {
    /// Create a new ORDER BY iterator sorting by `keys` (first = primary).
    pub fn new(input: Box<dyn ResultIterator>, keys: Vec<(SortKey, bool)>) -> Self {
        SortIterator {
            input: Some(input),
            keys,
            output: Vec::new().into_iter(),
            drained: false,
        }
    }

    fn drain(&mut self) -> Result<()> {
        let mut input = match self.input.take() {
            Some(i) => i,
            None => return Ok(()),
        };
        let mut rows: Vec<QueryRow> = Vec::new();
        while let Some(row) = input.next() {
            rows.push(row?);
        }
        rows.sort_by(|a, b| self.cmp_rows(a, b));
        self.output = rows.into_iter();
        Ok(())
    }

    fn cmp_rows(&self, a: &QueryRow, b: &QueryRow) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        for (key, descending) in &self.keys {
            let ord = Self::cmp_by_key(a, b, key, *descending);
            if ord != Ordering::Equal {
                return ord;
            }
        }
        Ordering::Equal
    }

    /// Compare two rows by a single key, applying openCypher null placement
    /// (nulls last for ASC, first for DESC).
    fn cmp_by_key(
        a: &QueryRow,
        b: &QueryRow,
        key: &SortKey,
        descending: bool,
    ) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        match key {
            SortKey::Property(prop) => match (row_value(a, prop), row_value(b, prop)) {
                (Some(x), Some(y)) => {
                    let ord = compare_property(x, y);
                    if descending { ord.reverse() } else { ord }
                }
                // openCypher: nulls last for ASC, first for DESC.
                (Some(_), None) => {
                    if descending {
                        Ordering::Greater
                    } else {
                        Ordering::Less
                    }
                }
                (None, Some(_)) => {
                    if descending {
                        Ordering::Less
                    } else {
                        Ordering::Greater
                    }
                }
                (None, None) => Ordering::Equal,
            },
            SortKey::Score => {
                let ord = a.score.partial_cmp(&b.score).unwrap_or(Ordering::Equal);
                if descending { ord.reverse() } else { ord }
            }
            SortKey::Timestamp => {
                let av = a.timestamp.map(|t| t.wallclock());
                let bv = b.timestamp.map(|t| t.wallclock());
                let ord = av.cmp(&bv);
                if descending { ord.reverse() } else { ord }
            }
        }
    }
}

impl ResultIterator for SortIterator {
    fn next(&mut self) -> Option<Result<QueryRow>> {
        if !self.drained {
            self.drained = true;
            if let Err(e) = self.drain() {
                return Some(Err(e));
            }
        }
        self.output.next().map(Ok)
    }
}

/// `Count` aggregate iterator: drains the input and yields a single computed
/// row with the total row count under the column name `count`.
pub struct CountIterator {
    input: Option<Box<dyn ResultIterator>>,
    done: bool,
}

impl CountIterator {
    /// Create a new count iterator over `input`.
    pub fn new(input: Box<dyn ResultIterator>) -> Self {
        CountIterator {
            input: Some(input),
            done: false,
        }
    }
}

impl ResultIterator for CountIterator {
    fn next(&mut self) -> Option<Result<QueryRow>> {
        if self.done {
            return None;
        }
        self.done = true;
        let mut input = self.input.take()?;
        let mut count: i64 = 0;
        while let Some(row) = input.next() {
            if let Err(e) = row {
                return Some(Err(e));
            }
            count += 1;
        }
        Some(Ok(QueryRow::from_columns(vec![(
            "count".to_string(),
            PropertyValue::Int(count),
        )])))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (usize::from(!self.done), Some(usize::from(!self.done)))
    }
}


#[cfg(test)]
mod tests;
