//! Current-state storage engine.
//!
//! This module implements the "hot path" storage for the current state of the
//! graph. It provides O(1) lookups and cache-friendly traversals optimized for
//! non-temporal queries.

use crate::core::graph::{Edge, Node};
use crate::core::id::{EdgeId, IdGenerator, NodeId, VersionId};
use crate::core::interning::{GLOBAL_INTERNER, InternedString};
use crate::core::property::PropertyMap;
use crate::core::temporal::Timestamp;
use crate::index::current::{AdjacencyGuard, CurrentIndexes};
use crate::index::vector::hnsw::{HnswConfig, HnswIndex};
use crate::index::vector::temporal::{TemporalVectorConfig, TemporalVectorIndex};
use crate::index::vector::{DistanceMetric, TemporalSearchResults, VectorIndex};
use crate::utils::error::{Result, StorageError};
use dashmap::DashMap;
use dashmap::mapref::entry::Entry;

/// Maximum number of vector-indexed properties allowed per database.
///
/// This limit prevents resource exhaustion from enabling too many vector indexes,
/// as each index consumes significant memory (1.5-2x the raw vector data size
/// due to HNSW graph overhead).
///
/// Override via configuration if your use case requires more properties.
pub const DEFAULT_MAX_VECTOR_PROPERTIES: usize = 10;
use parking_lot::RwLock;
use std::sync::Arc;

/// Statistics about current storage
#[derive(Debug, Clone)]
pub struct CurrentStats {
    /// Number of nodes
    pub node_count: usize,
    /// Number of edges
    pub edge_count: usize,
}

/// Statistics for adaptive over-fetch heuristic in filtered vector search.
///
/// Tracks the historical pass rate of label filters to dynamically adjust
/// the over-fetch multiplier. This improves performance by:
/// - Reducing over-fetch for dense labels (high pass rate)
/// - Increasing over-fetch for sparse labels (low pass rate)
///
/// Issue #334: Adaptive over-fetch strategy
#[derive(Debug)]
struct FilterStats {
    /// Number of searches performed for this label
    search_count: std::sync::atomic::AtomicU64,
    /// Total number of candidates fetched across all searches
    total_candidates: std::sync::atomic::AtomicU64,
    /// Total number of results returned after filtering
    total_results: std::sync::atomic::AtomicU64,
}

impl FilterStats {
    /// Create new filter statistics with zero counts.
    fn new() -> Self {
        FilterStats {
            search_count: std::sync::atomic::AtomicU64::new(0),
            total_candidates: std::sync::atomic::AtomicU64::new(0),
            total_results: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Record a search operation and its results.
    ///
    /// # Arguments
    ///
    /// * `candidates_fetched` - Number of candidates retrieved from HNSW
    /// * `results_returned` - Number of results after label filtering
    ///
    /// # Memory Ordering
    ///
    /// Uses `Relaxed` ordering because:
    /// - These are simple counters with no synchronization requirements
    /// - Exact ordering between increments doesn't affect correctness
    /// - Pass rate calculation tolerates slightly stale reads
    /// - Performance is critical (called on every filtered search)
    ///
    /// # Overflow Safety
    ///
    /// Overflow after 2^64 operations is not a realistic concern in practice.
    /// At 1 million searches/second, overflow would take ~584,000 years.
    /// Standard wrapping arithmetic is used for simplicity and performance.
    fn record_search(&self, candidates_fetched: usize, results_returned: usize) {
        use std::sync::atomic::Ordering;

        // Simple atomic increments - wrapping overflow at 2^64 is not a realistic concern
        self.search_count.fetch_add(1, Ordering::Relaxed);
        self.total_candidates
            .fetch_add(candidates_fetched as u64, Ordering::Relaxed);
        self.total_results
            .fetch_add(results_returned as u64, Ordering::Relaxed);
    }

    /// Calculate the adaptive over-fetch multiplier based on historical pass rate.
    ///
    /// # Returns
    ///
    /// A multiplier to apply to k (e.g., 10.0 means fetch k * 10 candidates).
    ///
    /// # Algorithm
    ///
    /// 1. Start with default multiplier (10.0) for first few searches
    /// 2. Calculate historical pass rate = results / candidates
    /// 3. Adjust multiplier inversely to pass rate:
    ///    - High pass rate (90%+) → lower multiplier (5-7)
    ///    - Medium pass rate (50%) → medium multiplier (10-15)
    ///    - Low pass rate (10%-) → higher multiplier (20-30)
    /// 4. Cap multiplier at reasonable bounds (5.0 - 50.0)
    fn get_adaptive_multiplier(&self) -> f64 {
        use std::sync::atomic::Ordering;

        const MIN_SEARCHES_FOR_ADAPTATION: u64 = 3;
        const DEFAULT_MULTIPLIER: f64 = 10.0;
        const MIN_MULTIPLIER: f64 = 5.0;
        const MAX_MULTIPLIER: f64 = 50.0;

        let search_count = self.search_count.load(Ordering::Relaxed);

        // Use default until we have enough data
        if search_count < MIN_SEARCHES_FOR_ADAPTATION {
            return DEFAULT_MULTIPLIER;
        }

        let total_candidates = self.total_candidates.load(Ordering::Relaxed);
        let total_results = self.total_results.load(Ordering::Relaxed);

        // Avoid division by zero
        if total_candidates == 0 {
            return DEFAULT_MULTIPLIER;
        }

        // Calculate historical pass rate
        // Clamp to 1.0 to handle race conditions where results might temporarily
        // exceed candidates due to concurrent atomic operations
        let pass_rate = (total_results as f64 / total_candidates as f64).min(1.0);

        // Adaptive formula: higher multiplier for lower pass rates
        // multiplier = base / sqrt(pass_rate)
        // This gives smooth adaptation:
        // - pass_rate=1.0 (100%) → multiplier=5
        // - pass_rate=0.5 (50%)  → multiplier=7
        // - pass_rate=0.1 (10%)  → multiplier=16
        // - pass_rate=0.01 (1%)  → multiplier=50
        let multiplier = MIN_MULTIPLIER / pass_rate.sqrt();

        // Clamp to reasonable bounds
        multiplier.clamp(MIN_MULTIPLIER, MAX_MULTIPLIER)
    }
}

/// Entry for a single vector index on a specific property.
///
/// Each property can have its own HNSW index with independent configuration
/// (dimensions, distance metric, etc.). This enables multi-property vector
/// indexing for use cases like separate title/body embeddings.
struct VectorIndexEntry {
    /// The HNSW index for this property
    index: Arc<HnswIndex>,
    /// Configuration used to create this index
    config: HnswConfig,
}

/// Information about a configured vector index.
///
/// Returned by [`CurrentStorage::list_vector_indexes`] to provide
/// metadata about all enabled vector indexes.
#[derive(Debug, Clone)]
pub struct VectorIndexInfo {
    /// The property name this index is configured for
    pub property_name: String,
    /// Number of dimensions in the vectors
    pub dimensions: usize,
    /// Distance metric used for similarity calculations
    pub distance_metric: DistanceMetric,
}

/// Internal state for vector indexing (legacy single-property).
///
/// TODO: Remove in next major version - replaced by multi-property DashMap.
struct VectorIndexState {
    index: Option<Arc<HnswIndex>>,
    property_name: Option<String>,
    config: Option<HnswConfig>,
}

impl VectorIndexState {
    fn new() -> Self {
        VectorIndexState {
            index: None,
            property_name: None,
            config: None,
        }
    }

    fn is_enabled(&self) -> bool {
        self.index.is_some()
    }
}

/// Entry for a temporal vector index (multi-property support).
struct TemporalVectorIndexEntry {
    /// The temporal vector index for this property
    index: Arc<TemporalVectorIndex>,
    /// Configuration used to create this index
    #[allow(dead_code)]
    config: TemporalVectorConfig,
}

/// Legacy internal state for temporal vector indexing.
/// Kept for backward compatibility with existing code paths.
struct TemporalVectorIndexState {
    index: Option<Arc<TemporalVectorIndex>>,
    property_name: Option<String>,
    config: Option<TemporalVectorConfig>,
}

impl TemporalVectorIndexState {
    fn new() -> Self {
        TemporalVectorIndexState {
            index: None,
            property_name: None,
            config: None,
        }
    }

    #[allow(dead_code)] // Kept for backward compatibility with legacy single-property API
    fn is_enabled(&self) -> bool {
        self.index.is_some()
    }
}

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
            "\navoiding the `Vec` allocation overhead of [`", $method_link, "`](Self::", $method_link, ").",
            "\n\n# Performance",
            "\n\n- **Zero allocation**: Holds an `AdjacencyGuard` (just an `Arc` clone, ~5-10ns)",
            "\n- **Lazy evaluation**: Only computes results as you iterate",
            "\n- **Cache-friendly**: Sequential access to CSR adjacency data",
            "\n\n# Issue #187",
            "\n\nThis iterator addresses the performance issue where edge traversal methods",
            "\nunnecessarily allocated a new `Vec<EdgeId>` on every call, adding 100-500ns",
            "\noverhead per traversal."
        )]
        pub struct $name {
            guard: AdjacencyGuard,
            index: usize,
        }

        impl $name {
            #[doc = concat!("Create a new iterator over ", $direction, " edges.")]
            #[inline]
            fn new(guard: AdjacencyGuard) -> Self {
                Self { guard, index: 0 }
            }
        }

        impl Iterator for $name {
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

        impl ExactSizeIterator for $name {}
        impl std::iter::FusedIterator for $name {}
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
            "\n[`", $method_link, "`](Self::", $method_link, ").",
            "\n\n# Performance",
            "\n\n- **Zero allocation**: Holds an `AdjacencyGuard` and pre-resolved label ID",
            "\n- **Lazy evaluation**: Filters as you iterate",
            "\n- **Early termination**: If label doesn't exist, returns empty iterator"
        )]
        pub struct $name {
            guard: AdjacencyGuard,
            index: usize,
            label_id: Option<InternedString>,
        }

        impl $name {
            #[doc = concat!("Create a new iterator over ", $direction, " edges with a specific label.")]
            #[inline]
            fn new(guard: AdjacencyGuard, label_id: Option<InternedString>) -> Self {
                Self {
                    guard,
                    index: 0,
                    label_id,
                }
            }
        }

        impl Iterator for $name {
            type Item = EdgeId;

            #[inline]
            fn next(&mut self) -> Option<Self::Item> {
                let label_id = self.label_id?;
                let remaining = &self.guard[self.index..];

                if let Some(pos) = remaining.iter().position(|e| e.label == label_id) {
                    self.index += pos + 1;
                    Some(remaining[pos].edge_id)
                } else {
                    self.index = self.guard.len();
                    None
                }
            }

            #[inline]
            fn size_hint(&self) -> (usize, Option<usize>) {
                let remaining = self.guard.len().saturating_sub(self.index);
                (0, Some(remaining))
            }
        }

        impl std::iter::FusedIterator for $name {}
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

/// Current-state storage engine.
///
/// This storage engine maintains the current version of all nodes and edges,
/// optimized for fast queries without temporal overhead. This is the "fast path"
/// that should achieve <1µs single-hop traversals.
pub struct CurrentStorage {
    /// Indexes for nodes and edges
    indexes: CurrentIndexes,
    /// ID generator for nodes
    node_id_gen: IdGenerator,
    /// ID generator for edges
    edge_id_gen: IdGenerator,
    /// ID generator for versions
    version_id_gen: IdGenerator,
    /// Multi-property vector indexes (Issue #389)
    /// Maps property name -> VectorIndexEntry
    vector_indexes: DashMap<String, VectorIndexEntry>,
    /// Vector index state (legacy single-property, for backward compatibility)
    vector_index_state: Arc<RwLock<VectorIndexState>>,
    /// Multi-property temporal vector indexes (Issue #389 fix)
    /// Maps property name -> TemporalVectorIndexEntry
    temporal_vector_indexes: DashMap<String, TemporalVectorIndexEntry>,
    /// Legacy temporal vector index state (for backward compatibility)
    temporal_vector_index_state: Arc<RwLock<TemporalVectorIndexState>>,
    /// Adaptive over-fetch statistics per label (Issue #334)
    /// Maps label -> FilterStats for tracking label-specific filter pass rates
    ///
    /// **Memory Considerations**: This map grows unbounded as new labels are encountered.
    /// For workloads with bounded label sets (typical case), memory usage is negligible
    /// (~100 bytes per unique label). For unbounded labels (e.g., using UUIDs as labels),
    /// memory usage scales linearly with unique label count (~50MB per 1M labels).
    ///
    /// **Recommendation**: Use a bounded set of labels for optimal performance.
    /// Future enhancement could add LRU eviction for unbounded scenarios.
    filter_stats: DashMap<String, Arc<FilterStats>>,
}

impl CurrentStorage {
    /// Create a new empty current storage.
    pub fn new() -> Self {
        CurrentStorage {
            indexes: CurrentIndexes::new(),
            node_id_gen: IdGenerator::new(),
            edge_id_gen: IdGenerator::new(),
            version_id_gen: IdGenerator::new(),
            vector_indexes: DashMap::new(),
            vector_index_state: Arc::new(RwLock::new(VectorIndexState::new())),
            temporal_vector_indexes: DashMap::new(),
            temporal_vector_index_state: Arc::new(RwLock::new(TemporalVectorIndexState::new())),
            filter_stats: DashMap::new(),
        }
    }

    /// Initialize the node ID generator with a specific starting value.
    ///
    /// This is used during recovery to ensure the ID generator continues from
    /// the maximum ID found in the WAL, preventing ID conflicts.
    ///
    /// # Arguments
    ///
    /// * `start` - The next ID to generate (typically max_id + 1)
    pub(crate) fn init_node_id_generator(&self, start: u64) {
        self.node_id_gen.reset_to(start);
    }

    /// Initialize the edge ID generator with a specific starting value.
    ///
    /// This is used during recovery to ensure the ID generator continues from
    /// the maximum ID found in the WAL, preventing ID conflicts.
    ///
    /// # Arguments
    ///
    /// * `start` - The next ID to generate (typically max_id + 1)
    pub(crate) fn init_edge_id_generator(&self, start: u64) {
        self.edge_id_gen.reset_to(start);
    }

    /// Initialize the version ID generator with a specific starting value.
    ///
    /// This is used during recovery to ensure the ID generator continues from
    /// the maximum ID found in the WAL, preventing ID conflicts.
    ///
    /// # Arguments
    ///
    /// * `start` - The next ID to generate (typically max_id + 1)
    #[inline]
    pub(crate) fn init_version_id_generator(&self, start: u64) {
        self.version_id_gen.reset_to(start);
    }

    /// Ensure the version ID generator's next value is at least the specified minimum.
    ///
    /// This is used during recovery when we need to account for version IDs from multiple
    /// sources (e.g., current storage and historical storage) without overwriting
    /// a higher value that was already set.
    ///
    /// # Arguments
    ///
    /// * `min_value` - The minimum next version ID to generate
    #[inline]
    pub(crate) fn ensure_version_id_generator_at_least(&self, min_value: u64) {
        self.version_id_gen.ensure_at_least(min_value);
    }

    /// Enable vector indexing for a specific property.
    ///
    /// Multiple properties can be indexed simultaneously with different configurations.
    /// Each property can have its own dimensions and distance metric.
    ///
    /// # Arguments
    ///
    /// * `property_name` - Name of the property containing vectors
    /// * `config` - HNSW index configuration
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - This specific property already has a vector index enabled
    /// - The maximum number of indexed properties ([`DEFAULT_MAX_VECTOR_PROPERTIES`]) is reached
    ///
    /// # Memory Usage
    ///
    /// Each vector index consumes approximately **1.5-2x** the raw vector data size due to
    /// HNSW graph overhead (neighbor lists, layer structure). For example:
    ///
    /// | Vectors | Dimensions | Raw Size | Index Size |
    /// |---------|------------|----------|------------|
    /// | 100K    | 384        | ~150 MB  | ~225-300 MB |
    /// | 1M      | 768        | ~3 GB    | ~4.5-6 GB   |
    /// | 1M      | 1536       | ~6 GB    | ~9-12 GB    |
    ///
    /// Plan capacity accordingly when enabling multiple property indexes.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Index title embeddings (384 dimensions)
    /// storage.enable_vector_index("title_embedding", config_384)?;
    /// // Index body embeddings (1536 dimensions) - different property, OK!
    /// storage.enable_vector_index("body_embedding", config_1536)?;
    /// ```
    pub fn enable_vector_index(&self, property_name: &str, config: HnswConfig) -> Result<()> {
        // Check property limit before attempting to add
        if self.vector_indexes.len() >= DEFAULT_MAX_VECTOR_PROPERTIES {
            return Err(crate::utils::error::Error::Vector(
                crate::utils::error::VectorError::IndexError(format!(
                    "Maximum number of vector-indexed properties ({}) reached. \
                     Cannot enable index for property '{}'",
                    DEFAULT_MAX_VECTOR_PROPERTIES, property_name
                )),
            ));
        }

        // Use atomic entry() API to avoid TOCTOU race condition
        match self.vector_indexes.entry(property_name.to_string()) {
            Entry::Occupied(_) => {
                return Err(crate::utils::error::Error::Vector(
                    crate::utils::error::VectorError::IndexError(format!(
                        "Vector index already enabled for property '{}'",
                        property_name
                    )),
                ));
            }
            Entry::Vacant(vacant) => {
                // Create the HNSW index
                let index = HnswIndex::new(config.clone())?;
                let entry = VectorIndexEntry {
                    index: Arc::new(index),
                    config: config.clone(),
                };
                vacant.insert(entry);
            }
        }

        // Update legacy single-property state for backward compatibility
        // Only stores property name/config for legacy API methods; actual index is in DashMap
        let mut state = self.vector_index_state.write();
        if !state.is_enabled() {
            // First index becomes the "default" for legacy API (find_similar, etc.)
            state.property_name = Some(property_name.to_string());
            state.config = Some(config);
            // Note: state.index is no longer used - we read from DashMap
        }

        Ok(())
    }

    /// Check if any vector indexing is enabled.
    ///
    /// Returns `true` if at least one property has a vector index.
    pub fn is_vector_index_enabled(&self) -> bool {
        !self.vector_indexes.is_empty() || self.vector_index_state.read().is_enabled()
    }

    /// Check if vector indexing is enabled for a specific property.
    ///
    /// Returns `true` if the property has a vector index configured.
    pub fn is_vector_index_enabled_for(&self, property_name: &str) -> bool {
        self.vector_indexes.contains_key(property_name)
    }

    /// Get the first/default property name that is currently indexed.
    ///
    /// Returns `Some(property_name)` if a vector index is enabled,
    /// or `None` if no index is configured.
    ///
    /// Note: For multi-property setups, use [`list_vector_indexes`] instead.
    pub fn get_indexed_property_name(&self) -> Option<String> {
        // First check legacy state for backward compatibility
        let legacy = self.vector_index_state.read().property_name.clone();
        if legacy.is_some() {
            return legacy;
        }
        // Fallback to first entry in multi-property map
        self.vector_indexes.iter().next().map(|r| r.key().clone())
    }

    /// List all configured vector indexes.
    ///
    /// Returns information about each enabled vector index including
    /// property name, dimensions, and distance metric.
    pub fn list_vector_indexes(&self) -> Vec<VectorIndexInfo> {
        self.vector_indexes
            .iter()
            .map(|entry| VectorIndexInfo {
                property_name: entry.key().clone(),
                dimensions: entry.value().config.dimensions,
                distance_metric: entry.value().config.metric,
            })
            .collect()
    }

    /// Check if a vector index is enabled for a specific property.
    ///
    /// # Arguments
    ///
    /// * `property_name` - The property name to check
    ///
    /// # Returns
    ///
    /// `true` if a vector index exists for this property, `false` otherwise.
    pub fn has_vector_index(&self, property_name: &str) -> bool {
        self.vector_indexes.contains_key(property_name)
    }

    /// Get the HNSW configuration for a specific property's vector index.
    ///
    /// # Arguments
    ///
    /// * `property_name` - The property name to get configuration for
    ///
    /// # Returns
    ///
    /// `Some(HnswConfig)` if a vector index exists for this property, `None` otherwise.
    pub fn get_hnsw_config_for(&self, property_name: &str) -> Option<HnswConfig> {
        self.vector_indexes
            .get(property_name)
            .map(|entry| entry.config.clone())
    }

    /// Get vector index configuration for checkpoint persistence.
    ///
    /// Returns the current vector index configuration if enabled, or disabled
    /// checkpoint data if no index is active.
    pub fn get_vector_index_config(
        &self,
    ) -> crate::storage::persistence::VectorIndexCheckpointData {
        use crate::storage::persistence::VectorIndexCheckpointData;

        let state = self.vector_index_state.read();

        if let (Some(config), Some(property_name)) =
            (state.config.clone(), state.property_name.clone())
        {
            VectorIndexCheckpointData::enabled(property_name, config)
        } else {
            VectorIndexCheckpointData::disabled()
        }
    }

    /// Register a vector index (used during index loading from disk).
    ///
    /// This directly inserts the index without any initialization logic.
    pub(crate) fn register_vector_index(
        &self,
        property_name: &str,
        index: crate::index::vector::HnswIndex,
        config: crate::index::vector::HnswConfig,
    ) {
        let index_arc = Arc::new(index);

        // Insert into multi-property map
        self.vector_indexes.insert(
            property_name.to_string(),
            VectorIndexEntry {
                index: Arc::clone(&index_arc),
                config: config.clone(),
            },
        );

        // Update legacy single-property state (for backward compatibility)
        let mut state = self.vector_index_state.write();
        state.index = Some(Arc::clone(&index_arc));
        state.property_name = Some(property_name.to_string());
        state.config = Some(config);
    }

    /// Get a reference to the HNSW index and its config for a specific property.
    ///
    /// Used for persistence operations. Returns (index, config, vector_count, mappings).
    #[allow(clippy::type_complexity)]
    pub(crate) fn get_vector_index_for_persistence(
        &self,
        property_name: &str,
    ) -> Option<(
        Arc<crate::index::vector::HnswIndex>,
        crate::index::vector::HnswConfig,
        usize,
        Vec<(u64, u64)>,
    )> {
        use crate::index::vector::VectorIndex;
        self.vector_indexes.get(property_name).map(|entry| {
            let index = entry.value().index.clone();
            let config = entry.value().config.clone();
            let count = index.len();

            // Extract ID mappings from the index
            let mappings = index.get_id_mappings();

            (index, config, count, mappings)
        })
    }

    /// Try to add a node's vectors to all enabled indexes.
    ///
    /// For each enabled vector index, checks if the node has that property
    /// and adds it to the index if so.
    ///
    /// Returns Ok(true) if any vector was indexed, Ok(false) if none applicable,
    /// Err on failure (will have already done partial work).
    fn try_index_vector(&self, node_id: NodeId, properties: &PropertyMap) -> Result<bool> {
        let mut indexed_any = false;

        // Index in all multi-property indexes
        for entry in self.vector_indexes.iter() {
            let prop_name = entry.key();
            if let Some(vector) = properties.get(prop_name).and_then(|v| v.as_vector()) {
                let index = Arc::clone(&entry.value().index);
                index.add(node_id, vector)?;
                indexed_any = true;
            }
        }

        Ok(indexed_any)
    }

    /// Try to remove a node from all vector indexes.
    ///
    /// Returns Ok(true) if removed from any index, Ok(false) if not applicable.
    fn try_remove_from_index(&self, node_id: NodeId) -> Result<bool> {
        let mut removed_any = false;

        // Remove from all multi-property indexes
        for entry in self.vector_indexes.iter() {
            let index = Arc::clone(&entry.value().index);
            // Remove may fail if node wasn't in this index, which is OK
            if index.remove(node_id).is_ok() {
                removed_any = true;
            }
        }

        Ok(removed_any)
    }

    /// Update the vector indexes when node properties change.
    ///
    /// For each enabled vector index, handles add/remove/update based on
    /// whether the property changed.
    fn update_vector_index(
        &self,
        node_id: NodeId,
        new_props: &PropertyMap,
        old_props: &PropertyMap,
    ) -> Result<()> {
        // Update all multi-property indexes
        for entry in self.vector_indexes.iter() {
            let prop_name = entry.key();
            let index = Arc::clone(&entry.value().index);

            let old_vec = old_props.get(prop_name).and_then(|v| v.as_vector());
            let new_vec = new_props.get(prop_name).and_then(|v| v.as_vector());

            match (old_vec, new_vec) {
                (None, None) => {} // No change for this property
                (None, Some(v)) => {
                    index.add(node_id, v)?;
                }
                (Some(_), None) => {
                    let _ = index.remove(node_id); // May not exist, ignore error
                }
                (Some(o), Some(n)) => {
                    if o != n {
                        // Note: HnswIndex::add() is an upsert operation (remove + add internally)
                        index.add(node_id, n)?;
                    }
                }
            }
        }

        Ok(())
    }
    /// Create a node with the given label and properties.
    ///
    /// Returns the ID of the newly created node.
    pub fn create_node(&self, label: &str, properties: PropertyMap) -> Result<NodeId> {
        let node_id = NodeId::new_unchecked(self.node_id_gen.next()?);
        let version_id = VersionId::new_unchecked(self.version_id_gen.next()?);
        let label_interned = GLOBAL_INTERNER.intern(label)?;

        let node = Node::new(node_id, label_interned, properties.clone(), version_id);
        self.indexes.insert_node(node.clone());

        // Try to index vector property if enabled
        if let Err(e) = self.try_index_vector(node_id, &properties) {
            // Rollback: remove node from indexes
            self.indexes.remove_node(node_id);
            return Err(e);
        }

        Ok(node_id)
    }

    /// Create an edge between two nodes.
    ///
    /// Returns the ID of the newly created edge.
    pub fn create_edge(
        &self,
        source: NodeId,
        target: NodeId,
        label: &str,
        properties: PropertyMap,
    ) -> Result<EdgeId> {
        // Verify nodes exist
        if !self.indexes.contains_node(source) {
            return Err(StorageError::NodeNotFound(source).into());
        }
        if !self.indexes.contains_node(target) {
            return Err(StorageError::NodeNotFound(target).into());
        }

        let edge_id = EdgeId::new_unchecked(self.edge_id_gen.next()?);
        let version_id = VersionId::new_unchecked(self.version_id_gen.next()?);
        let label_interned = GLOBAL_INTERNER.intern(label)?;

        let edge = Edge::new(
            edge_id,
            label_interned,
            source,
            target,
            properties,
            version_id,
        );
        self.indexes.insert_edge(edge);

        // Adjacency indexes are now rebuilt lazily on first access (see CurrentIndexes)
        // This eliminates O(n² log n) performance regression for batch operations

        Ok(edge_id)
    }

    /// Get a node by ID.
    pub fn get_node(&self, id: NodeId) -> Result<Node> {
        self.indexes
            .get_node(id)
            .ok_or_else(|| StorageError::NodeNotFound(id).into())
    }

    /// Get an edge by ID.
    pub fn get_edge(&self, id: EdgeId) -> Result<Edge> {
        self.indexes
            .get_edge(id)
            .ok_or_else(|| StorageError::EdgeNotFound(id).into())
    }

    /// Delete a node.
    ///
    /// Note: This does not delete edges connected to the node.
    /// See issue #364 for cascade delete option.
    pub fn delete_node(&self, id: NodeId) -> Result<Node> {
        self.indexes
            .remove_node(id)
            .ok_or_else(|| StorageError::NodeNotFound(id).into())
    }

    /// Delete an edge.
    pub fn delete_edge(&self, id: EdgeId) -> Result<Edge> {
        let edge = self
            .indexes
            .remove_edge(id)
            .ok_or(StorageError::EdgeNotFound(id))?;

        // Adjacency indexes are now rebuilt lazily on first access (see CurrentIndexes)
        // This eliminates O(n² log n) performance regression for batch operations

        Ok(edge)
    }

    // Direct insert/update/delete methods for transaction commit
    // These methods are used by WriteTransaction to apply buffered changes

    /// Insert a node directly (used by WriteTransaction).
    /// Does not generate IDs - caller must provide them.
    pub fn insert_node_direct(&self, node: Node, timestamp: Timestamp) -> Result<()> {
        // CRITICAL: Index vector BEFORE inserting node. If vector indexing fails,
        // we have not modified any graph state, so we can safely return error without rollback.
        // This prevents the VS-030 bug where transaction-created nodes bypassed indexing,
        // causing them to be missing from HNSW index and invisible to find_similar queries.
        self.try_index_vector(node.id, &node.properties)?;

        // Index in temporal vector index if enabled
        self.try_index_temporal_vector(node.id, &node.properties, timestamp)?;

        // Vector indexing succeeded, now insert the node into the main indexes.
        self.indexes.insert_node(node);

        Ok(())
    }

    /// Insert an edge directly (used by WriteTransaction).
    /// Does not generate IDs or rebuild adjacency - caller must handle.
    pub fn insert_edge_direct(&self, edge: Edge) -> Result<()> {
        self.indexes.insert_edge(edge);
        Ok(())
    }

    /// Update a node directly (used by WriteTransaction).
    pub fn update_node_direct(&self, node: Node, timestamp: Timestamp) -> Result<()> {
        // Save old node for potential rollback
        let old_node = self.indexes.get_node(node.id);

        // Update node
        self.indexes.insert_node(node.clone());

        // Update vector index
        if let Some(ref old) = old_node
            && let Err(e) = self.update_vector_index(node.id, &node.properties, &old.properties)
        {
            // Rollback: restore the original node
            self.indexes.insert_node(old.clone());
            return Err(e);
        }

        // Update temporal vector index if enabled
        self.try_index_temporal_vector(node.id, &node.properties, timestamp)?;

        Ok(())
    }

    /// Update an edge directly (used by WriteTransaction).
    pub fn update_edge_direct(&self, edge: Edge) -> Result<()> {
        // Remove old version and insert new
        self.indexes.insert_edge(edge);
        Ok(())
    }

    /// Delete a node directly (used by WriteTransaction).
    pub fn delete_node_direct(&self, id: NodeId, timestamp: Timestamp) -> Result<()> {
        self.indexes
            .remove_node(id)
            .ok_or(StorageError::NodeNotFound(id))?;

        // Best-effort vector index removal (ignore errors)
        let _ = self.try_remove_from_index(id);

        // Remove from temporal vector index if enabled
        let _ = self.try_remove_temporal_vector(id, timestamp);

        Ok(())
    }

    /// Delete an edge directly (used by WriteTransaction).
    pub fn delete_edge_direct(&self, id: EdgeId) -> Result<()> {
        self.indexes
            .remove_edge(id)
            .ok_or(StorageError::EdgeNotFound(id))?;
        Ok(())
    }

    /// Rebuild adjacency indexes from current edges.
    ///
    /// This should be called after batch edge operations to update the
    /// adjacency indexes for efficient graph traversal.
    ///
    /// # Concurrency Safety
    ///
    /// This method is safe to call concurrently with read operations:
    /// - Uses `RwLock` on adjacency indexes for safe concurrent access
    /// - Readers can continue traversing old indexes while rebuild occurs
    /// - New index is swapped in atomically when rebuild completes
    /// - No stale reads: readers either see old (consistent) or new (consistent) state
    ///
    /// However, concurrent writes should be serialized at a higher level
    /// (e.g., through transaction isolation) to prevent race conditions.
    ///
    /// # Performance
    ///
    /// Complexity: O(E log E) where E is the total number of edges.
    /// This operation acquires a write lock, which will block concurrent
    /// readers for the duration of the rebuild (~microseconds for small graphs,
    /// ~milliseconds for graphs with 10K+ edges).
    pub fn rebuild_adjacency(&self) {
        self.indexes.rebuild_adjacency();
    }

    /// Get all outgoing edges from a node.
    ///
    /// This is the critical "hot path" operation that must be fast.
    #[inline]
    pub fn get_outgoing_edges(&self, source: NodeId) -> Vec<EdgeId> {
        self.indexes
            .get_outgoing(source)
            .iter()
            .map(|entry| entry.edge_id)
            .collect()
    }

    /// Get all incoming edges to a node.
    #[inline]
    pub fn get_incoming_edges(&self, target: NodeId) -> Vec<EdgeId> {
        self.indexes
            .get_incoming(target)
            .iter()
            .map(|entry| entry.edge_id)
            .collect()
    }

    /// Get outgoing edges with a specific label.
    #[inline]
    pub fn get_outgoing_edges_with_label(&self, source: NodeId, label: &str) -> Vec<EdgeId> {
        let label_id = match GLOBAL_INTERNER.get_id(label) {
            Some(id) => id,
            None => return Vec::new(), // Label doesn't exist
        };

        self.indexes
            .get_outgoing_with_label(source, label_id)
            .into_iter()
            .map(|entry| entry.edge_id)
            .collect()
    }

    /// Get incoming edges with a specific label.
    #[inline]
    pub fn get_incoming_edges_with_label(&self, target: NodeId, label: &str) -> Vec<EdgeId> {
        let label_id = match GLOBAL_INTERNER.get_id(label) {
            Some(id) => id,
            None => return Vec::new(),
        };

        self.indexes
            .get_incoming_with_label(target, label_id)
            .into_iter()
            .map(|entry| entry.edge_id)
            .collect()
    }

    /// Get all outgoing edges from a node as an iterator.
    ///
    /// This is a zero-allocation alternative to [`Self::get_outgoing_edges`] that returns
    /// an iterator instead of collecting into a `Vec`. Use this for performance-critical
    /// traversals where you don't need to store all edges.
    ///
    /// # Performance
    ///
    /// - No `Vec` allocation (saves 100-500ns per call)
    /// - Lazy evaluation - only computes what you consume
    /// - Can be short-circuited with `.take()`, `.find()`, etc.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Get first outgoing edge without allocating a Vec
    /// let first_edge = storage.get_outgoing_edges_iter(node).next();
    ///
    /// // Count edges without allocation
    /// let count = storage.get_outgoing_edges_iter(node).count();
    /// ```
    #[inline]
    pub fn get_outgoing_edges_iter(&self, source: NodeId) -> OutgoingEdgesIter {
        OutgoingEdgesIter::new(self.indexes.get_outgoing(source))
    }

    /// Get all incoming edges to a node as an iterator.
    ///
    /// This is a zero-allocation alternative to [`Self::get_incoming_edges`] that returns
    /// an iterator instead of collecting into a `Vec`.
    ///
    /// See [`Self::get_outgoing_edges_iter`] for performance details and usage examples.
    #[inline]
    pub fn get_incoming_edges_iter(&self, target: NodeId) -> IncomingEdgesIter {
        IncomingEdgesIter::new(self.indexes.get_incoming(target))
    }

    /// Get outgoing edges with a specific label as an iterator.
    ///
    /// This is a zero-allocation alternative to [`Self::get_outgoing_edges_with_label`].
    ///
    /// Returns an empty iterator if the label doesn't exist.
    #[inline]
    pub fn get_outgoing_edges_with_label_iter(
        &self,
        source: NodeId,
        label: &str,
    ) -> OutgoingEdgesWithLabelIter {
        let label_id = GLOBAL_INTERNER.get_id(label);
        OutgoingEdgesWithLabelIter::new(self.indexes.get_outgoing(source), label_id)
    }

    /// Get incoming edges with a specific label as an iterator.
    ///
    /// This is a zero-allocation alternative to [`Self::get_incoming_edges_with_label`].
    ///
    /// Returns an empty iterator if the label doesn't exist.
    #[inline]
    pub fn get_incoming_edges_with_label_iter(
        &self,
        target: NodeId,
        label: &str,
    ) -> IncomingEdgesWithLabelIter {
        let label_id = GLOBAL_INTERNER.get_id(label);
        IncomingEdgesWithLabelIter::new(self.indexes.get_incoming(target), label_id)
    }

    /// Export outgoing CSR adjacency data for persistence.
    pub fn export_outgoing_csr(&self) -> (Vec<u64>, Vec<u64>) {
        self.indexes.export_outgoing_csr()
    }

    /// Export incoming CSR adjacency data for persistence.
    pub fn export_incoming_csr(&self) -> (Vec<u64>, Vec<u64>) {
        self.indexes.export_incoming_csr()
    }

    /// Import CSR adjacency data from persistence.
    ///
    /// This bypasses the need to rebuild adjacency structures from scratch.
    pub fn import_csr(
        &self,
        outgoing_offsets: Vec<u64>,
        outgoing_edge_ids: Vec<u64>,
        incoming_offsets: Vec<u64>,
        incoming_edge_ids: Vec<u64>,
    ) {
        self.indexes.import_csr(
            outgoing_offsets,
            outgoing_edge_ids,
            incoming_offsets,
            incoming_edge_ids,
        );
    }

    /// Get the number of nodes.
    #[inline]
    pub fn node_count(&self) -> usize {
        self.indexes.node_count()
    }

    /// Get the number of edges.
    #[inline]
    pub fn edge_count(&self) -> usize {
        self.indexes.edge_count()
    }

    /// Get the out-degree of a node.
    #[inline]
    pub fn out_degree(&self, node: NodeId) -> usize {
        self.indexes.out_degree(node)
    }

    /// Get the in-degree of a node.
    #[inline]
    pub fn in_degree(&self, node: NodeId) -> usize {
        self.indexes.in_degree(node)
    }

    /// Helper method to prepare for vector search.
    /// Returns the Arc<HnswIndex> and the query vector.
    ///
    /// This uses the "default" property (first enabled) for backward compatibility.
    /// For multi-property searches, use `find_similar_in` instead.
    fn prepare_vector_search(&self, query_node_id: NodeId) -> Result<(Arc<HnswIndex>, Vec<f32>)> {
        // Get the default property name from legacy state
        let state = self.vector_index_state.read();
        let prop_name = state.property_name.as_ref().ok_or_else(|| {
            crate::utils::error::Error::Vector(crate::utils::error::VectorError::IndexError(
                "Vector index is not enabled".to_string(),
            ))
        })?;
        let prop_name = prop_name.clone();
        drop(state);

        // Get the index from DashMap (where actual data is stored)
        let entry = self.vector_indexes.get(&prop_name).ok_or_else(|| {
            crate::utils::error::Error::Vector(crate::utils::error::VectorError::IndexError(
                format!("Vector index not found for property '{}'", prop_name),
            ))
        })?;

        let query_node = self
            .indexes
            .get_node(query_node_id)
            .ok_or(StorageError::NodeNotFound(query_node_id))?;
        let query_vec_ref = query_node
            .properties
            .get(&prop_name)
            .ok_or_else(|| StorageError::PropertyNotFound(prop_name.clone()))?
            .as_vector()
            .ok_or_else(|| {
                crate::utils::error::Error::Vector(
                    crate::utils::error::VectorError::InvalidVector {
                        reason: "Property is not a vector".to_string(),
                    },
                )
            })?;

        let query_vector: Vec<f32> = query_vec_ref.to_vec();
        let index = Arc::clone(&entry.value().index);
        drop(query_node);

        Ok((index, query_vector))
    }

    /// Get or create filter statistics for a label (Issue #334).
    ///
    /// Returns an Arc to the FilterStats for the given label, creating it if needed.
    /// This enables adaptive over-fetch multiplier calculation based on historical
    /// filter pass rates.
    fn get_or_create_filter_stats(&self, label: &str) -> Arc<FilterStats> {
        self.filter_stats
            .entry(label.to_string())
            .or_insert_with(|| Arc::new(FilterStats::new()))
            .value()
            .clone()
    }

    /// Calculate adaptive over-fetch candidates for filtered search (Issue #334).
    ///
    /// Returns the number of candidates to fetch and the FilterStats for recording results.
    /// This method centralizes the adaptive over-fetch logic used by all filtered search methods.
    ///
    /// # Arguments
    ///
    /// * `k` - Number of results requested by the user
    /// * `label` - Label to filter by
    ///
    /// # Returns
    ///
    /// Tuple of (candidates_to_fetch, stats) where:
    /// - `candidates_to_fetch` is the adaptive number of candidates to retrieve from HNSW
    /// - `stats` is the FilterStats for recording search results
    ///
    /// # Algorithm
    ///
    /// - Uses adaptive multiplier based on historical pass rate (5x to 50x)
    /// - Guarantees minimum of k + 20 candidates
    /// - Caps maximum at k + 1000 to prevent excessive memory usage
    fn calculate_adaptive_candidates(&self, k: usize, label: &str) -> (usize, Arc<FilterStats>) {
        const MIN_ABSOLUTE_OVERFETCH: usize = 20;
        const MAX_ABSOLUTE_OVERFETCH: usize = 1000;

        let stats = self.get_or_create_filter_stats(label);
        let multiplier = stats.get_adaptive_multiplier();
        let candidates = ((k as f64 * multiplier) as usize)
            .max(k + MIN_ABSOLUTE_OVERFETCH)
            .min(k + MAX_ABSOLUTE_OVERFETCH);

        (candidates, stats)
    }

    /// Get filter statistics for a label (test-only helper).
    ///
    /// Returns the current statistics (search_count, total_candidates, total_results)
    /// for the given label, or None if no searches have been performed yet.
    ///
    /// This is used for testing to verify that adaptive learning is working correctly.
    pub(crate) fn get_filter_stats(&self, label: &str) -> Option<(u64, u64, u64)> {
        use std::sync::atomic::Ordering;

        self.filter_stats.get(label).map(|entry| {
            let stats = entry.value();
            (
                stats.search_count.load(Ordering::Relaxed),
                stats.total_candidates.load(Ordering::Relaxed),
                stats.total_results.load(Ordering::Relaxed),
            )
        })
    }

    /// Find k most similar nodes to the query node based on vector similarity.
    ///
    /// Returns a list of (NodeId, score) pairs sorted by similarity (highest first).
    /// The query node itself is excluded from results.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Vector index is not enabled
    /// - Query node is not found
    /// - Query node does not have the indexed vector property
    /// - The property is not a vector
    pub fn find_similar(&self, query_node_id: NodeId, k: usize) -> Result<Vec<(NodeId, f32)>> {
        let (index, query_vector) = self.prepare_vector_search(query_node_id)?;

        let mut results = index.search(&query_vector, k + 1)?;
        results.retain(|(id, _)| *id != query_node_id);
        results.truncate(k);
        Ok(results)
    }

    /// Find k most similar nodes with a specific label.
    ///
    /// Returns a list of (NodeId, score) pairs sorted by similarity (highest first).
    /// Only nodes with the specified label are returned. The query node is excluded.
    pub fn find_similar_with_label(
        &self,
        query_node_id: NodeId,
        label: &str,
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>> {
        let (index, query_vector) = self.prepare_vector_search(query_node_id)?;
        let label_id = GLOBAL_INTERNER.intern(label)?;

        let (candidates_to_fetch, stats) = self.calculate_adaptive_candidates(k, label);

        let mut results =
            index.search_with_filter(&query_vector, candidates_to_fetch, |node_id| {
                self.indexes
                    .get_node(*node_id)
                    .map(|n| n.label == label_id)
                    .unwrap_or(false)
            })?;

        results.retain(|(id, _)| *id != query_node_id);
        let results_count = results.len();
        results.truncate(k);

        // Record search statistics for adaptive learning (Issue #334)
        stats.record_search(candidates_to_fetch, results_count);

        Ok(results)
    }

    /// Find k most similar nodes to a raw embedding vector.
    ///
    /// This is useful when searching with embeddings that don't correspond to any
    /// existing node in the graph (e.g., query embeddings from external sources).
    ///
    /// # Arguments
    ///
    /// * `embedding` - The query embedding vector
    /// * `k` - Maximum number of results to return
    ///
    /// # Returns
    ///
    /// A list of (NodeId, similarity_score) pairs sorted by similarity (highest first).
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Vector index is not enabled
    /// - Embedding dimensions don't match the indexed property
    pub fn find_similar_by_embedding(
        &self,
        embedding: &[f32],
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>> {
        let index = self.prepare_vector_search_raw(embedding)?;
        let results = index.search(embedding, k)?;
        Ok(results)
    }

    /// Find k most similar nodes with a specific label to a raw embedding vector.
    ///
    /// Like `find_similar_by_embedding()`, but filters results to only include
    /// nodes with the specified label.
    ///
    /// # Arguments
    ///
    /// * `embedding` - The query embedding vector
    /// * `label` - Only return nodes with this label
    /// * `k` - Maximum number of results to return
    ///
    /// # Returns
    ///
    /// A list of (NodeId, similarity_score) pairs sorted by similarity (highest first).
    /// All results have the specified label.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Vector index is not enabled
    /// - Embedding dimensions don't match the indexed property
    pub fn find_similar_by_embedding_with_label(
        &self,
        embedding: &[f32],
        label: &str,
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>> {
        let index = self.prepare_vector_search_raw(embedding)?;

        // Intern the label for efficient comparison
        let label_id = GLOBAL_INTERNER.intern(label)?;

        let (candidates_to_fetch, stats) = self.calculate_adaptive_candidates(k, label);

        // Filter during HNSW traversal for better performance
        let mut results = index.search_with_filter(embedding, candidates_to_fetch, |node_id| {
            self.indexes
                .get_node(*node_id)
                .is_some_and(|n| n.label == label_id)
        })?;

        let results_count = results.len();
        // Truncate to requested k (search_with_filter may return more)
        results.truncate(k);

        // Record search statistics for adaptive learning (Issue #334)
        stats.record_search(candidates_to_fetch, results_count);

        Ok(results)
    }

    /// Find k most similar nodes to a raw embedding in a specific property's index.
    ///
    /// This is the property-specific version of `find_similar_by_embedding()`.
    /// Use this when you have multiple vector indexes and need to search a specific one.
    ///
    /// # Arguments
    ///
    /// * `property_name` - The property with the vector index to search
    /// * `embedding` - The query embedding vector
    /// * `k` - Maximum number of results to return
    ///
    /// # Returns
    ///
    /// A list of (NodeId, similarity_score) pairs sorted by similarity (highest first).
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - No vector index is enabled for the specified property
    /// - Embedding dimensions don't match the indexed property
    pub fn find_similar_by_embedding_in(
        &self,
        property_name: &str,
        embedding: &[f32],
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>> {
        let entry = self.vector_indexes.get(property_name).ok_or_else(|| {
            crate::utils::error::Error::Vector(crate::utils::error::VectorError::IndexError(
                format!(
                    "No vector index enabled for property '{}'. Call enable_vector_index() first.",
                    property_name
                ),
            ))
        })?;

        // Validate embedding dimensions
        let expected_dims = entry.value().config.dimensions;
        if embedding.len() != expected_dims {
            return Err(crate::utils::error::Error::Vector(
                crate::utils::error::VectorError::DimensionMismatch {
                    expected: expected_dims,
                    actual: embedding.len(),
                },
            ));
        }

        let results = entry.value().index.search(embedding, k)?;
        Ok(results)
    }

    /// Find k most similar nodes with a label to a raw embedding in a specific property's index.
    ///
    /// This is the property-specific version of `find_similar_by_embedding_with_label()`.
    ///
    /// # Arguments
    ///
    /// * `property_name` - The property with the vector index to search
    /// * `embedding` - The query embedding vector
    /// * `label` - Only return nodes with this label
    /// * `k` - Maximum number of results to return
    pub fn find_similar_by_embedding_in_with_label(
        &self,
        property_name: &str,
        embedding: &[f32],
        label: &str,
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>> {
        let entry = self.vector_indexes.get(property_name).ok_or_else(|| {
            crate::utils::error::Error::Vector(crate::utils::error::VectorError::IndexError(
                format!(
                    "No vector index enabled for property '{}'. Call enable_vector_index() first.",
                    property_name
                ),
            ))
        })?;

        // Validate embedding dimensions
        let expected_dims = entry.value().config.dimensions;
        if embedding.len() != expected_dims {
            return Err(crate::utils::error::Error::Vector(
                crate::utils::error::VectorError::DimensionMismatch {
                    expected: expected_dims,
                    actual: embedding.len(),
                },
            ));
        }

        let label_id = GLOBAL_INTERNER.intern(label)?;

        let (candidates_to_fetch, stats) = self.calculate_adaptive_candidates(k, label);

        let mut results =
            entry
                .value()
                .index
                .search_with_filter(embedding, candidates_to_fetch, |node_id| {
                    self.indexes
                        .get_node(*node_id)
                        .is_some_and(|n| n.label == label_id)
                })?;

        let results_count = results.len();
        results.truncate(k);

        // Record search statistics for adaptive learning (Issue #334)
        stats.record_search(candidates_to_fetch, results_count);

        Ok(results)
    }

    // ========================================================================
    // Multi-Property Vector Search Methods (Issue #389)
    // ========================================================================

    /// Find k most similar nodes in a specific property's vector index.
    ///
    /// Use this method when you have multiple vector indexes and need to search
    /// a specific one. The property must have a vector index enabled.
    ///
    /// # Arguments
    ///
    /// * `property_name` - The indexed property to search
    /// * `query_node_id` - The node to find similar nodes for
    /// * `k` - Maximum number of results to return
    ///
    /// # Returns
    ///
    /// A list of (NodeId, similarity_score) pairs sorted by similarity (highest first).
    /// The query node itself is excluded from results.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - No vector index is enabled for the specified property
    /// - Query node is not found
    /// - Query node does not have the specified property
    /// - The property value is not a vector
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Search title embeddings for similar nodes
    /// let similar = storage.find_similar_in("title_embedding", node_id, 10)?;
    ///
    /// // Search body embeddings (different property, potentially different results)
    /// let similar_body = storage.find_similar_in("body_embedding", node_id, 10)?;
    /// ```
    pub fn find_similar_in(
        &self,
        property_name: &str,
        query_node_id: NodeId,
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>> {
        // Get the index for this property
        let entry = self.vector_indexes.get(property_name).ok_or_else(|| {
            crate::utils::error::Error::Vector(crate::utils::error::VectorError::IndexError(
                format!(
                    "No vector index enabled for property '{}'. Call enable_vector_index() first.",
                    property_name
                ),
            ))
        })?;

        // Get the query node and its vector
        let query_node = self
            .indexes
            .get_node(query_node_id)
            .ok_or(StorageError::NodeNotFound(query_node_id))?;

        let query_vec = query_node
            .get_property(property_name)
            .and_then(|v| v.as_vector())
            .ok_or_else(|| {
                crate::utils::error::Error::Vector(crate::utils::error::VectorError::IndexError(
                    format!(
                        "Node {} does not have vector property '{}'",
                        query_node_id.as_u64(),
                        property_name
                    ),
                ))
            })?;

        // Perform the search
        let index = Arc::clone(&entry.value().index);
        let mut results = index.search(query_vec, k + 1)?;
        results.retain(|(id, _)| *id != query_node_id);
        results.truncate(k);
        Ok(results)
    }

    /// Search a specific property's vector index with a raw embedding.
    ///
    /// Use this method when searching with embeddings that don't correspond to
    /// any existing node in the graph (e.g., query embeddings from external sources).
    ///
    /// # Arguments
    ///
    /// * `property_name` - The indexed property to search
    /// * `embedding` - The query embedding vector
    /// * `k` - Maximum number of results to return
    ///
    /// # Returns
    ///
    /// A list of (NodeId, similarity_score) pairs sorted by similarity (highest first).
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - No vector index is enabled for the specified property
    /// - Embedding dimensions don't match the index configuration
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Search with external embedding
    /// let query = embed_text("search query");
    /// let results = storage.search_vectors_in("title_embedding", &query, 10)?;
    /// ```
    pub fn search_vectors_in(
        &self,
        property_name: &str,
        embedding: &[f32],
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>> {
        // Get the index for this property
        let entry = self.vector_indexes.get(property_name).ok_or_else(|| {
            crate::utils::error::Error::Vector(crate::utils::error::VectorError::IndexError(
                format!(
                    "No vector index enabled for property '{}'. Call enable_vector_index() first.",
                    property_name
                ),
            ))
        })?;

        // Validate embedding dimensions
        let expected_dims = entry.value().config.dimensions;
        if embedding.len() != expected_dims {
            return Err(crate::utils::error::Error::Vector(
                crate::utils::error::VectorError::DimensionMismatch {
                    expected: expected_dims,
                    actual: embedding.len(),
                },
            ));
        }

        // Perform the search
        let index = Arc::clone(&entry.value().index);
        let results = index.search(embedding, k)?;
        Ok(results)
    }

    /// Helper method to prepare for raw embedding vector search.
    /// Returns the Arc<HnswIndex> and validates the embedding.
    ///
    /// This uses the "default" property (first enabled) for backward compatibility.
    /// For multi-property searches, use `search_vectors_in` instead.
    fn prepare_vector_search_raw(&self, embedding: &[f32]) -> Result<Arc<HnswIndex>> {
        // Get the default property name from legacy state
        let state = self.vector_index_state.read();
        let prop_name = state.property_name.as_ref().ok_or_else(|| {
            crate::utils::error::Error::Vector(crate::utils::error::VectorError::IndexError(
                "Vector index is not enabled. Call enable_vector_index() first.".to_string(),
            ))
        })?;
        let prop_name = prop_name.clone();
        drop(state);

        // Get the index from DashMap (where actual data is stored)
        let entry = self.vector_indexes.get(&prop_name).ok_or_else(|| {
            crate::utils::error::Error::Vector(crate::utils::error::VectorError::IndexError(
                format!("Vector index not found for property '{}'", prop_name),
            ))
        })?;

        // Validate embedding dimensions match index
        let expected_dims = entry.value().config.dimensions;
        if embedding.len() != expected_dims {
            return Err(crate::utils::error::Error::Vector(
                crate::utils::error::VectorError::DimensionMismatch {
                    expected: expected_dims,
                    actual: embedding.len(),
                },
            ));
        }

        let index = Arc::clone(&entry.value().index);

        Ok(index)
    }

    // ========================================================================
    // Temporal Vector Indexing (Phase 3)
    // ========================================================================

    /// Enable temporal vector indexing for a specific property.
    ///
    /// Once enabled, vector changes will be tracked over time using snapshot-based
    /// indexing, enabling point-in-time vector queries and semantic drift tracking.
    ///
    /// # Arguments
    ///
    /// * `property_name` - Name of the property containing vectors
    /// * `config` - Temporal vector index configuration
    ///
    /// # Errors
    ///
    /// Returns an error if temporal vector indexing is already enabled.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use gallifreydb::index::vector::temporal::{TemporalVectorConfig, SnapshotStrategy};
    /// use gallifreydb::index::vector::HnswConfig;
    ///
    /// let hnsw_config = HnswConfig::new(384, DistanceMetric::Cosine);
    /// let temporal_config = TemporalVectorConfig::default_with_hnsw(hnsw_config);
    /// storage.enable_temporal_vector_index("embedding", temporal_config)?;
    /// ```
    pub fn enable_temporal_vector_index(
        &self,
        property_name: &str,
        config: TemporalVectorConfig,
    ) -> Result<()> {
        // Check if already enabled for this specific property
        if self.temporal_vector_indexes.contains_key(property_name) {
            return Err(crate::utils::error::Error::Vector(
                crate::utils::error::VectorError::IndexError(format!(
                    "Temporal vector index is already enabled for property '{}'",
                    property_name
                )),
            ));
        }

        // Create temporal vector index wrapped in Arc for sharing
        let index = Arc::new(TemporalVectorIndex::new(config.clone())?);

        // Insert into multi-property DashMap
        self.temporal_vector_indexes.insert(
            property_name.to_string(),
            TemporalVectorIndexEntry {
                index: Arc::clone(&index),
                config: config.clone(),
            },
        );

        // Also update legacy state for backward compatibility
        // (uses the most recently added index)
        let mut state = self.temporal_vector_index_state.write();
        state.index = Some(index);
        state.property_name = Some(property_name.to_string());
        state.config = Some(config);

        Ok(())
    }

    /// Check if temporal vector indexing is enabled for any property.
    pub fn is_temporal_vector_index_enabled(&self) -> bool {
        !self.temporal_vector_indexes.is_empty()
    }

    /// Check if temporal vector indexing is enabled for a specific property.
    pub fn is_temporal_vector_index_enabled_for(&self, property_name: &str) -> bool {
        self.temporal_vector_indexes.contains_key(property_name)
    }

    /// List all property names that have temporal vector indexes enabled.
    ///
    /// Returns a vector of property names that have temporal vector indexing configured.
    pub fn list_temporal_vector_indexes(&self) -> Vec<String> {
        self.temporal_vector_indexes
            .iter()
            .map(|entry| entry.key().clone())
            .collect()
    }

    /// Get a reference to the temporal vector index for a specific property.
    ///
    /// Returns `None` if temporal vector indexing is not enabled for that property.
    pub(crate) fn get_temporal_vector_index_for(
        &self,
        property_name: &str,
    ) -> Option<Arc<TemporalVectorIndex>> {
        self.temporal_vector_indexes
            .get(property_name)
            .map(|entry| Arc::clone(&entry.index))
    }

    /// Get a reference to the temporal vector index if enabled (legacy single-property API).
    ///
    /// Returns `None` if temporal vector indexing is not enabled.
    /// For multi-property support, use `get_temporal_vector_index_for` instead.
    pub(crate) fn get_temporal_vector_index(&self) -> Option<Arc<TemporalVectorIndex>> {
        let state = self.temporal_vector_index_state.read();
        state.index.clone()
    }

    /// Find k most similar nodes at a specific point in time.
    ///
    /// Returns nodes similar to the query embedding as they existed at the given timestamp.
    ///
    /// # Arguments
    ///
    /// * `embedding` - Query vector
    /// * `k` - Number of results
    /// * `timestamp` - Point in time to query
    ///
    /// # Returns
    ///
    /// Vector of (NodeId, similarity) pairs sorted by similarity (descending).
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Temporal vector index is not enabled
    /// - No snapshot exists at or before the timestamp
    /// - Embedding dimensions don't match the indexed property
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Find similar documents as they existed in the past
    /// let query_embedding = vec![0.1; 384];
    /// let timestamp = 1234567890000000; // microseconds since epoch
    /// let results = storage.find_similar_as_of(&query_embedding, 10, timestamp)?;
    /// ```
    pub fn find_similar_as_of(
        &self,
        embedding: &[f32],
        k: usize,
        timestamp: Timestamp,
    ) -> Result<Vec<(NodeId, f32)>> {
        let state = self.temporal_vector_index_state.read();
        let index = state.index.as_ref().ok_or_else(|| {
            crate::utils::error::Error::Vector(crate::utils::error::VectorError::IndexError(
                "Temporal vector index is not enabled. Call enable_temporal_vector_index() first."
                    .to_string(),
            ))
        })?;

        index.find_similar_as_of(embedding, k, timestamp)
    }

    /// Find k most similar nodes at a specific point in time for a specific property.
    ///
    /// This is the property-specific version of [`find_similar_as_of()`].
    /// It validates that the requested property matches the property for which
    /// the temporal vector index was enabled.
    ///
    /// # Arguments
    ///
    /// * `property_name` - The property containing the vector embeddings
    /// * `embedding` - Query vector to find similar vectors to
    /// * `k` - Number of results to return
    /// * `timestamp` - The point in time to query
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Temporal vector index is not enabled
    /// - The property name doesn't match the indexed property
    /// - Query embedding dimensions don't match
    /// - No snapshot exists at the given timestamp
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Find similar documents as they existed in the past for a specific property
    /// let query_embedding = vec![0.1; 384];
    /// let timestamp = 1234567890000000;
    /// let results = storage.find_similar_as_of_in("content_embedding", &query_embedding, 10, timestamp)?;
    /// ```
    pub fn find_similar_as_of_in(
        &self,
        property_name: &str,
        embedding: &[f32],
        k: usize,
        timestamp: crate::core::temporal::Timestamp,
    ) -> Result<Vec<(crate::core::NodeId, f32)>> {
        // Look up the temporal index for this specific property
        let index = self
            .get_temporal_vector_index_for(property_name)
            .ok_or_else(|| {
                crate::utils::error::Error::Vector(crate::utils::error::VectorError::IndexError(
                    format!(
                        "No temporal vector index enabled for property '{}'. \
                     Call db.vector_index(\"{}\").temporal(...).enable() first.",
                        property_name, property_name
                    ),
                ))
            })?;

        index.find_similar_as_of(embedding, k, timestamp)
    }

    /// Track semantic drift for a node over time in a specific property's temporal index.
    ///
    /// This method tracks how a node's embedding has changed relative to a reference
    /// embedding over time. It validates that the requested property matches the
    /// property for which the temporal vector index was enabled.
    ///
    /// # Arguments
    ///
    /// * `property_name` - The property containing the vector embeddings
    /// * `node_id` - The node to track drift for
    /// * `reference_embedding` - Reference vector to measure drift against
    /// * `time_range` - The time range to search for drift
    ///
    /// # Returns
    ///
    /// A vector of (timestamp, drift_score) pairs showing how the node's embedding
    /// drifted from the reference at each snapshot time.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Temporal vector index is not enabled
    /// - The property name doesn't match the indexed property
    /// - Reference embedding dimensions don't match
    pub fn track_drift_in(
        &self,
        property_name: &str,
        node_id: crate::core::NodeId,
        reference_embedding: &[f32],
        time_range: crate::core::temporal::TimeRange,
    ) -> Result<Vec<(crate::core::temporal::Timestamp, f32)>> {
        // Look up the temporal index for this specific property (multi-property support)
        let index = self
            .get_temporal_vector_index_for(property_name)
            .ok_or_else(|| {
                crate::utils::error::Error::Vector(crate::utils::error::VectorError::IndexError(
                    format!(
                        "No temporal vector index enabled for property '{}'. \
                     Call db.vector_index(\"{}\").temporal(...).enable() first.",
                        property_name, property_name
                    ),
                ))
            })?;

        index.track_semantic_drift(node_id, reference_embedding, time_range)
    }

    /// Get the semantic evolution of a node's embedding over time in a specific property.
    ///
    /// Returns the actual embedding vectors at each snapshot timestamp.
    pub fn semantic_evolution_in(
        &self,
        property_name: &str,
        node_id: crate::core::NodeId,
        time_range: crate::core::temporal::TimeRange,
    ) -> Result<Vec<(crate::core::temporal::Timestamp, std::sync::Arc<[f32]>)>> {
        // Look up the temporal index for this specific property (multi-property support)
        let index = self
            .get_temporal_vector_index_for(property_name)
            .ok_or_else(|| {
                crate::utils::error::Error::Vector(crate::utils::error::VectorError::IndexError(
                    format!(
                        "No temporal vector index enabled for property '{}'. \
                     Call db.vector_index(\"{}\").temporal(...).enable() first.",
                        property_name, property_name
                    ),
                ))
            })?;

        index.semantic_evolution(node_id, time_range)
    }

    /// Find all nodes with semantic drift above a threshold in a specific property.
    ///
    /// Scans all nodes and identifies those whose embeddings have changed
    /// by more than the specified threshold over the time range.
    pub fn find_drift_in(
        &self,
        property_name: &str,
        threshold: f32,
        time_range: crate::core::temporal::TimeRange,
        metric: crate::index::vector::temporal::DriftMetric,
    ) -> Result<Vec<(crate::core::NodeId, f32)>> {
        // Look up the temporal index for this specific property (multi-property support)
        let index = self
            .get_temporal_vector_index_for(property_name)
            .ok_or_else(|| {
                crate::utils::error::Error::Vector(crate::utils::error::VectorError::IndexError(
                    format!(
                        "No temporal vector index enabled for property '{}'. \
                     Call db.vector_index(\"{}\").temporal(...).enable() first.",
                        property_name, property_name
                    ),
                ))
            })?;

        index.find_semantic_drift(threshold, time_range, metric)
    }

    /// Find k most similar nodes across a time range.
    ///
    /// Returns results for each snapshot within the time range, showing how
    /// semantic similarity evolved over time.
    ///
    /// # Arguments
    ///
    /// * `embedding` - Query vector
    /// * `k` - Number of results per snapshot
    /// * `time_range` - Time range to query
    ///
    /// # Returns
    ///
    /// Vector of (timestamp, results) pairs where results are Vec<(NodeId, similarity)>.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use gallifreydb::core::temporal::TimeRange;
    ///
    /// // Track how similar documents changed over time
    /// let query = vec![0.1; 384];
    /// let time_range = TimeRange::between(start_ts, end_ts);
    /// let results = storage.find_similar_in_range(&query, 10, time_range)?;
    /// for (timestamp, similar_nodes) in results {
    ///     println!("At timestamp {}: {} similar nodes", timestamp, similar_nodes.len());
    /// }
    /// ```
    pub fn find_similar_in_range(
        &self,
        embedding: &[f32],
        k: usize,
        time_range: crate::core::temporal::TimeRange,
    ) -> Result<TemporalSearchResults> {
        let state = self.temporal_vector_index_state.read();
        let index = state.index.as_ref().ok_or_else(|| {
            crate::utils::error::Error::Vector(crate::utils::error::VectorError::IndexError(
                "Temporal vector index is not enabled. Call enable_temporal_vector_index() first."
                    .to_string(),
            ))
        })?;

        index.find_similar_in_range(embedding, k, time_range)
    }

    /// Notify the temporal vector index of a transaction.
    ///
    /// This should be called after committing a transaction to trigger snapshot
    /// creation based on the configured strategy.
    pub fn on_temporal_vector_transaction(&self) -> Result<()> {
        let state = self.temporal_vector_index_state.read();
        if let Some(index) = &state.index {
            index.on_transaction()?;
        }
        Ok(())
    }

    /// Helper to index a vector in the temporal index.
    fn try_index_temporal_vector(
        &self,
        node_id: NodeId,
        properties: &PropertyMap,
        timestamp: Timestamp,
    ) -> Result<()> {
        let state = self.temporal_vector_index_state.read();
        if let Some(index) = &state.index
            && let Some(prop_name) = &state.property_name
            && let Some(vector) = properties.get(prop_name).and_then(|v| v.as_vector())
        {
            index.add(node_id, vector, timestamp)?;
        }
        Ok(())
    }

    /// Helper to remove a vector from the temporal index.
    fn try_remove_temporal_vector(&self, node_id: NodeId, timestamp: Timestamp) -> Result<()> {
        let state = self.temporal_vector_index_state.read();
        if let Some(index) = &state.index {
            index.remove(node_id, timestamp)?;
        }
        Ok(())
    }

    /// Get statistics about the current storage
    pub fn stats(&self) -> CurrentStats {
        CurrentStats {
            node_count: self.node_count(),
            edge_count: self.edge_count(),
        }
    }

    // ========================================================================
    // Query Executor Support Methods (VS-060)
    // ========================================================================

    /// Get all node IDs in the current storage.
    ///
    /// This is used by the query executor for full node scans.
    /// For large graphs, prefer using label-filtered scans instead.
    pub fn get_all_node_ids(&self) -> Vec<NodeId> {
        self.indexes.iter_nodes().map(|n| n.id).collect()
    }

    /// Get nodes by label.
    ///
    /// Returns an iterator over all nodes with the given label.
    /// This is more efficient than scanning all nodes and filtering.
    pub fn get_nodes_by_label(&self, label: &str) -> Vec<Node> {
        let label_id = match crate::core::interning::GLOBAL_INTERNER.get_id(label) {
            Some(id) => id,
            None => return Vec::new(), // Label doesn't exist
        };
        self.indexes
            .iter_nodes()
            .filter(|n| n.label == label_id)
            .collect()
    }

    /// Get the name of the property used for vector indexing.
    ///
    /// Returns `None` if vector indexing is not enabled.
    /// This is used by the query executor for vector reranking operations.
    pub fn get_vector_property_name(&self) -> Option<String> {
        self.vector_index_state.read().property_name.clone()
    }

    /// Get node counts grouped by label.
    ///
    /// Returns an iterator of (interned_label, count) pairs.
    /// Used by the query planner for cost estimation and cardinality estimation.
    pub fn label_counts(&self) -> Vec<(crate::core::interning::InternedString, usize)> {
        use std::collections::HashMap;
        let mut counts: HashMap<crate::core::interning::InternedString, usize> = HashMap::new();
        for node in self.indexes.iter_nodes() {
            *counts.entry(node.label).or_insert(0) += 1;
        }
        counts.into_iter().collect()
    }

    /// Get average out-degree across all nodes.
    ///
    /// Used by the query planner for cost estimation.
    pub fn avg_out_degree(&self) -> f64 {
        let node_count = self.node_count();
        if node_count == 0 {
            return 0.0;
        }
        self.edge_count() as f64 / node_count as f64
    }

    /// Get target node IDs from outgoing edges (used for traversal iterators).
    ///
    /// Returns the target node IDs of all outgoing edges from the source node.
    pub fn get_outgoing_targets(&self, source: NodeId) -> Vec<NodeId> {
        self.indexes
            .get_outgoing(source)
            .iter()
            .map(|entry| entry.target)
            .collect()
    }

    /// Get target node IDs from outgoing edges with a specific label.
    pub fn get_outgoing_targets_with_label(&self, source: NodeId, label: &str) -> Vec<NodeId> {
        let label_id = match GLOBAL_INTERNER.get_id(label) {
            Some(id) => id,
            None => return Vec::new(),
        };
        self.indexes
            .get_outgoing_with_label(source, label_id)
            .into_iter()
            .map(|entry| entry.target)
            .collect()
    }

    /// Get source node IDs from incoming edges (used for traversal iterators).
    ///
    /// Returns the source node IDs of all incoming edges to the target node.
    /// Note: For incoming edges, the "target" field in AdjacencyEntry represents
    /// the source node (the node the edge is coming from).
    pub fn get_incoming_sources(&self, target: NodeId) -> Vec<NodeId> {
        self.indexes
            .get_incoming(target)
            .iter()
            .map(|entry| entry.target) // target field stores the source for incoming edges
            .collect()
    }

    /// Get source node IDs from incoming edges with a specific label.
    pub fn get_incoming_sources_with_label(&self, target: NodeId, label: &str) -> Vec<NodeId> {
        let label_id = match GLOBAL_INTERNER.get_id(label) {
            Some(id) => id,
            None => return Vec::new(),
        };
        self.indexes
            .get_incoming_with_label(target, label_id)
            .into_iter()
            .map(|entry| entry.target) // target field stores the source for incoming edges
            .collect()
    }

    /// Get the number of vectors in the HNSW index.
    ///
    /// Used by the query planner for statistics.
    pub fn vector_count(&self) -> usize {
        let state = self.vector_index_state.read();
        state.index.as_ref().map(|idx| idx.len()).unwrap_or(0)
    }

    /// Iterate over all nodes (for persistence).
    ///
    /// This is a helper method for index persistence that provides
    /// access to all nodes in the current storage.
    ///
    /// Returns an iterator to avoid allocating a Vec for large graphs,
    /// improving memory efficiency during persistence operations.
    pub(crate) fn all_nodes(&self) -> impl Iterator<Item = Node> + '_ {
        self.indexes.iter_nodes()
    }

    /// Iterate over all edges (for persistence).
    ///
    /// This is a helper method for index persistence that provides
    /// access to all edges in the current storage.
    ///
    /// Returns an iterator to avoid allocating a Vec for large graphs,
    /// improving memory efficiency during persistence operations.
    pub(crate) fn all_edges(&self) -> impl Iterator<Item = Edge> + '_ {
        self.indexes.iter_edges()
    }
}

impl Default for CurrentStorage {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_create_node() {
        let storage = CurrentStorage::new();

        let props = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .build();

        let node_id = storage.create_node("Person", props).unwrap();

        assert_eq!(storage.node_count(), 1);

        let node = storage.get_node(node_id).unwrap();
        assert_eq!(node.id, node_id);
        assert_eq!(
            node.get_property("name").and_then(|v| v.as_str()),
            Some("Alice")
        );
    }

    #[test]
    fn test_create_edge() {
        let storage = CurrentStorage::new();

        let alice = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let bob = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();

        let edge_id = storage
            .create_edge(
                alice,
                bob,
                "KNOWS",
                PropertyMapBuilder::new().insert("since", 2020i64).build(),
            )
            .unwrap();

        assert_eq!(storage.edge_count(), 1);

        let edge = storage.get_edge(edge_id).unwrap();
        assert_eq!(edge.source, alice);
        assert_eq!(edge.target, bob);
        assert_eq!(
            edge.get_property("since").and_then(|v| v.as_int()),
            Some(2020)
        );
    }

    #[test]
    fn test_create_edge_invalid_nodes() {
        let storage = CurrentStorage::new();

        let result = storage.create_edge(
            NodeId::new(999).unwrap(),
            NodeId::new(1000).unwrap(),
            "KNOWS",
            PropertyMapBuilder::new().build(),
        );

        assert!(result.is_err());
    }

    #[test]
    fn test_graph_traversal() {
        let storage = CurrentStorage::new();

        // Create nodes
        let n0 = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let n1 = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let n2 = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();

        // Create edges
        storage
            .create_edge(n0, n1, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();
        storage
            .create_edge(n0, n2, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();
        storage
            .create_edge(n1, n2, "FOLLOWS", PropertyMapBuilder::new().build())
            .unwrap();

        // Test outgoing edges
        let outgoing = storage.get_outgoing_edges(n0);
        assert_eq!(outgoing.len(), 2);

        // Test incoming edges
        let incoming = storage.get_incoming_edges(n2);
        assert_eq!(incoming.len(), 2);

        // Test degree
        assert_eq!(storage.out_degree(n0), 2);
        assert_eq!(storage.in_degree(n2), 2);
    }

    #[test]
    fn test_labeled_edges() {
        let storage = CurrentStorage::new();

        let n0 = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let n1 = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let n2 = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();

        storage
            .create_edge(n0, n1, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();
        storage
            .create_edge(n0, n2, "FOLLOWS", PropertyMapBuilder::new().build())
            .unwrap();

        // Get only KNOWS edges
        let knows_edges = storage.get_outgoing_edges_with_label(n0, "KNOWS");
        assert_eq!(knows_edges.len(), 1);

        // Get only FOLLOWS edges
        let follows_edges = storage.get_outgoing_edges_with_label(n0, "FOLLOWS");
        assert_eq!(follows_edges.len(), 1);

        // Non-existent label
        let none_edges = storage.get_outgoing_edges_with_label(n0, "LOVES");
        assert_eq!(none_edges.len(), 0);
    }

    #[test]
    fn test_delete_node() {
        let storage = CurrentStorage::new();

        let node_id = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();

        assert_eq!(storage.node_count(), 1);

        let deleted = storage.delete_node(node_id).unwrap();
        assert_eq!(deleted.id, node_id);
        assert_eq!(storage.node_count(), 0);

        // Second delete should fail
        assert!(storage.delete_node(node_id).is_err());
    }

    #[test]
    fn test_delete_edge() {
        let storage = CurrentStorage::new();

        let n0 = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let n1 = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();

        let edge_id = storage
            .create_edge(n0, n1, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();

        assert_eq!(storage.edge_count(), 1);
        assert_eq!(storage.out_degree(n0), 1);

        storage.delete_edge(edge_id).unwrap();

        assert_eq!(storage.edge_count(), 0);
        assert_eq!(storage.out_degree(n0), 0);
    }

    // ========================================================================
    // Vector Property Tests (VS-011)
    // ========================================================================

    #[test]
    fn test_create_node_with_vector_property() {
        let storage = CurrentStorage::new();

        // Create a node with an embedding vector
        let embedding = vec![0.1f32, 0.2, 0.3, 0.4, 0.5];
        let props = PropertyMapBuilder::new()
            .insert("name", "Document")
            .insert_vector("embedding", &embedding)
            .build();

        let node_id = storage.create_node("Document", props).unwrap();

        // Retrieve and verify
        let node = storage.get_node(node_id).unwrap();
        assert_eq!(
            node.get_property("name").and_then(|v| v.as_str()),
            Some("Document")
        );

        assert_eq!(
            node.get_property("embedding").and_then(|v| v.as_vector()),
            Some(&embedding[..])
        );
    }

    #[test]
    fn test_create_node_with_high_dimensional_vector() {
        let storage = CurrentStorage::new();

        // Create a 384-dimensional vector (common embedding size)
        let embedding: Vec<f32> = (0..384).map(|i| i as f32 / 384.0).collect();
        let props = PropertyMapBuilder::new()
            .insert_vector("embedding", &embedding)
            .build();

        let node_id = storage.create_node("Embedding", props).unwrap();

        let node = storage.get_node(node_id).unwrap();
        assert_eq!(
            node.get_property("embedding").and_then(|v| v.as_vector()),
            Some(&embedding[..])
        );
    }

    #[test]
    fn test_create_edge_with_vector_property() {
        let storage = CurrentStorage::new();

        // Create nodes
        let n1 = storage
            .create_node("Entity", PropertyMapBuilder::new().build())
            .unwrap();
        let n2 = storage
            .create_node("Entity", PropertyMapBuilder::new().build())
            .unwrap();

        // Create edge with a relationship embedding
        let edge_embedding = vec![0.5f32, -0.3, 0.8];
        let props = PropertyMapBuilder::new()
            .insert("weight", 0.95f64)
            .insert_vector("embedding", &edge_embedding)
            .build();

        let edge_id = storage.create_edge(n1, n2, "RELATES_TO", props).unwrap();

        // Retrieve and verify
        let edge = storage.get_edge(edge_id).unwrap();
        assert_eq!(
            edge.get_property("weight").and_then(|v| v.as_float()),
            Some(0.95)
        );

        assert_eq!(
            edge.get_property("embedding").and_then(|v| v.as_vector()),
            Some(&edge_embedding[..])
        );
    }

    #[test]
    fn test_update_node_vector_property() {
        let storage = CurrentStorage::new();

        // Create node with initial embedding
        let initial_embedding = vec![0.1f32, 0.2, 0.3, 0.0];
        let props = PropertyMapBuilder::new()
            .insert("name", "Document")
            .insert_vector("embedding", &initial_embedding)
            .build();

        let node_id = storage.create_node("Document", props).unwrap();

        // Get node, update embedding, and save
        let mut node = storage.get_node(node_id).unwrap();

        // Update with new embedding
        let updated_embedding = vec![0.9f32, 0.8, 0.7, 0.0];
        let new_props = PropertyMapBuilder::new()
            .insert("name", "Document")
            .insert_vector("embedding", &updated_embedding)
            .build();
        node.properties = new_props;

        storage.update_node_direct(node, 1000.into()).unwrap();

        // Verify update
        let updated_node = storage.get_node(node_id).unwrap();
        assert_eq!(
            updated_node
                .get_property("embedding")
                .and_then(|v| v.as_vector()),
            Some(&updated_embedding[..])
        );
    }

    #[test]
    fn test_update_edge_vector_property() {
        let storage = CurrentStorage::new();

        let n1 = storage
            .create_node("Entity", PropertyMapBuilder::new().build())
            .unwrap();
        let n2 = storage
            .create_node("Entity", PropertyMapBuilder::new().build())
            .unwrap();

        // Create edge with initial embedding
        let initial_embedding = vec![1.0f32, 0.0];
        let props = PropertyMapBuilder::new()
            .insert_vector("embedding", &initial_embedding)
            .build();

        let edge_id = storage.create_edge(n1, n2, "RELATES_TO", props).unwrap();

        // Update edge embedding
        let mut edge = storage.get_edge(edge_id).unwrap();
        let updated_embedding = vec![0.0f32, 1.0];
        edge.properties = PropertyMapBuilder::new()
            .insert_vector("embedding", &updated_embedding)
            .build();

        storage.update_edge_direct(edge).unwrap();

        // Verify
        let updated_edge = storage.get_edge(edge_id).unwrap();
        assert_eq!(
            updated_edge
                .get_property("embedding")
                .and_then(|v| v.as_vector()),
            Some(&updated_embedding[..])
        );
    }

    #[test]
    fn test_create_node_with_multiple_vector_properties() {
        let storage = CurrentStorage::new();

        // Node with multiple embeddings (e.g., different model embeddings)
        let text_embedding = vec![0.1f32, 0.2, 0.3, 0.4];
        let image_embedding = vec![0.5f32, 0.6, 0.7, 0.8];
        let props = PropertyMapBuilder::new()
            .insert("content", "multimodal content")
            .insert_vector("text_embedding", &text_embedding)
            .insert_vector("image_embedding", &image_embedding)
            .build();

        let node_id = storage.create_node("MultimodalDoc", props).unwrap();

        let node = storage.get_node(node_id).unwrap();
        assert_eq!(
            node.get_property("text_embedding")
                .and_then(|v| v.as_vector()),
            Some(&text_embedding[..])
        );
        assert_eq!(
            node.get_property("image_embedding")
                .and_then(|v| v.as_vector()),
            Some(&image_embedding[..])
        );
    }

    #[test]
    fn test_create_node_with_empty_vector() {
        let storage = CurrentStorage::new();

        // Empty vector should be allowed
        let empty_embedding: Vec<f32> = vec![];
        let props = PropertyMapBuilder::new()
            .insert_vector("embedding", &empty_embedding)
            .build();

        let node_id = storage.create_node("EmptyVec", props).unwrap();

        let node = storage.get_node(node_id).unwrap();
        assert_eq!(
            node.get_property("embedding").and_then(|v| v.as_vector()),
            Some(&empty_embedding[..])
        );
    }

    #[test]
    fn test_create_node_with_normalized_vector() {
        let storage = CurrentStorage::new();

        // Vector with normalized values (common for embeddings)
        let normalized_embedding = vec![0.5773503f32, 0.5773503, 0.5773503, 0.0]; // unit vector
        let props = PropertyMapBuilder::new()
            .insert_vector("embedding", &normalized_embedding)
            .build();

        let node_id = storage.create_node("NormalizedDoc", props).unwrap();

        let node = storage.get_node(node_id).unwrap();
        let retrieved = node
            .get_property("embedding")
            .and_then(|v| v.as_vector())
            .expect("Embedding property should exist and be a vector");

        // Verify magnitude is approximately 1.0
        let magnitude: f32 = retrieved.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((magnitude - 1.0).abs() < 1e-5);
    }
    // ========================================================================
    // Vector Index Integration Tests (VS-030)
    // ========================================================================

    #[test]
    fn test_enable_vector_index() {
        use crate::index::vector::DistanceMetric;
        let storage = CurrentStorage::new();
        assert!(!storage.is_vector_index_enabled());

        let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
        storage.enable_vector_index("embedding", config).unwrap();
        assert!(storage.is_vector_index_enabled());
    }

    #[test]
    fn test_enable_vector_index_twice_fails() {
        use crate::index::vector::DistanceMetric;
        let storage = CurrentStorage::new();

        let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
        storage
            .enable_vector_index("embedding", config.clone())
            .unwrap();

        let result = storage.enable_vector_index("embedding", config);
        assert!(result.is_err());
    }

    #[test]
    fn test_auto_index_on_create() {
        use crate::index::vector::DistanceMetric;
        let storage = CurrentStorage::new();

        let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
        storage.enable_vector_index("embedding", config).unwrap();

        let embedding = vec![1.0f32, 0.0, 0.0, 0.0];
        let props = PropertyMapBuilder::new()
            .insert_vector("embedding", &embedding)
            .build();
        let node_id = storage.create_node("Document", props).unwrap();

        let node = storage.get_node(node_id).unwrap();
        assert_eq!(
            node.get_property("embedding").and_then(|v| v.as_vector()),
            Some(&embedding[..])
        );
    }

    #[test]
    fn test_auto_index_dimension_mismatch_rolls_back() {
        use crate::index::vector::DistanceMetric;
        let storage = CurrentStorage::new();

        let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
        storage.enable_vector_index("embedding", config).unwrap();

        // Wrong dimension - 2D instead of 3D
        let wrong_embedding = vec![1.0f32, 0.0];
        let props = PropertyMapBuilder::new()
            .insert_vector("embedding", &wrong_embedding)
            .build();

        let result = storage.create_node("Document", props);
        assert!(result.is_err());
        assert_eq!(storage.node_count(), 0); // Rollback worked
    }

    #[test]
    fn test_find_similar() {
        use crate::index::vector::DistanceMetric;
        let storage = CurrentStorage::new();

        let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
        storage.enable_vector_index("embedding", config).unwrap();

        let v1 = vec![1.0f32, 0.0, 0.0, 0.0];
        let v2 = vec![0.9f32, 0.1, 0.0, 0.0];
        let v3 = vec![0.0f32, 1.0, 0.0, 0.0];

        let node1 = storage
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &v1)
                    .build(),
            )
            .unwrap();
        let node2 = storage
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &v2)
                    .build(),
            )
            .unwrap();
        let _node3 = storage
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &v3)
                    .build(),
            )
            .unwrap();

        let results = storage.find_similar(node1, 2).unwrap();
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|(id, _)| *id != node1));
        assert_eq!(results[0].0, node2); // Most similar
    }

    #[test]
    fn test_find_similar_with_label() {
        use crate::index::vector::DistanceMetric;
        let storage = CurrentStorage::new();

        let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
        storage.enable_vector_index("embedding", config).unwrap();

        let v1 = vec![1.0f32, 0.0, 0.0, 0.0];
        let v2 = vec![0.9f32, 0.1, 0.0, 0.0];
        let v3 = vec![0.8f32, 0.2, 0.0, 0.0];

        let node1 = storage
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &v1)
                    .build(),
            )
            .unwrap();
        let node2 = storage
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &v2)
                    .build(),
            )
            .unwrap();
        let _node3 = storage
            .create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &v3)
                    .build(),
            )
            .unwrap();

        let results = storage.find_similar_with_label(node1, "Person", 2).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, node2);
    }

    #[test]
    fn test_find_similar_index_not_enabled() {
        let storage = CurrentStorage::new();
        let v1 = vec![1.0f32, 0.0, 0.0, 0.0];
        let node1 = storage
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &v1)
                    .build(),
            )
            .unwrap();

        let result = storage.find_similar(node1, 2);
        assert!(result.is_err());
    }

    #[test]
    fn test_find_similar_node_not_found() {
        use crate::index::vector::DistanceMetric;
        let storage = CurrentStorage::new();

        let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
        storage.enable_vector_index("embedding", config).unwrap();

        let result = storage.find_similar(NodeId::new(999).unwrap(), 2);
        assert!(result.is_err());
    }

    #[test]
    fn test_find_similar_property_not_found() {
        use crate::index::vector::DistanceMetric;
        let storage = CurrentStorage::new();

        let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
        storage.enable_vector_index("embedding", config).unwrap();

        let node1 = storage
            .create_node(
                "Doc",
                PropertyMapBuilder::new().insert("name", "test").build(),
            )
            .unwrap();
        let result = storage.find_similar(node1, 2);
        assert!(result.is_err());
    }

    #[test]
    fn test_update_node_updates_index() {
        use crate::index::vector::DistanceMetric;
        let storage = CurrentStorage::new();

        let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
        storage.enable_vector_index("embedding", config).unwrap();

        let v1 = vec![1.0f32, 0.0, 0.0, 0.0];
        let v2 = vec![0.0f32, 1.0, 0.0, 0.0];

        let node1 = storage
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &v1)
                    .build(),
            )
            .unwrap();
        let node2 = storage
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &v2)
                    .build(),
            )
            .unwrap();

        // Update node1 to be similar to node2
        let v1_updated = vec![0.1f32, 0.9, 0.0, 0.0];
        let mut node1_obj = storage.get_node(node1).unwrap();
        node1_obj.properties = PropertyMapBuilder::new()
            .insert_vector("embedding", &v1_updated)
            .build();
        storage.update_node_direct(node1_obj, 2000.into()).unwrap();

        let results = storage.find_similar(node2, 1).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, node1);
    }

    #[test]
    fn test_delete_node_removes_from_index() {
        use crate::index::vector::DistanceMetric;
        let storage = CurrentStorage::new();

        let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
        storage.enable_vector_index("embedding", config).unwrap();

        let v1 = vec![1.0f32, 0.0, 0.0, 0.0];
        let v2 = vec![0.9f32, 0.1, 0.0, 0.0];

        let node1 = storage
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &v1)
                    .build(),
            )
            .unwrap();
        let node2 = storage
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &v2)
                    .build(),
            )
            .unwrap();

        storage.delete_node_direct(node2, 3000.into()).unwrap();

        let results = storage.find_similar(node1, 2).unwrap();
        assert_eq!(results.len(), 0);
    }

    #[test]
    fn test_create_node_without_vector_property() {
        use crate::index::vector::DistanceMetric;
        let storage = CurrentStorage::new();

        let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
        storage.enable_vector_index("embedding", config).unwrap();

        // Create node without vector - should succeed
        let node1 = storage
            .create_node(
                "Doc",
                PropertyMapBuilder::new().insert("name", "test").build(),
            )
            .unwrap();
        assert_eq!(storage.node_count(), 1);
        let node = storage.get_node(node1).unwrap();
        assert_eq!(
            node.get_property("name").and_then(|v| v.as_str()),
            Some("test")
        );
    }

    // ========================================================================
    // Tests for Issue #24: delete_node/delete_edge with &self (P3-2)
    // ========================================================================

    /// Test that delete_node can be called with an immutable reference.
    ///
    /// This test verifies that delete_node(&self) works correctly since
    /// the underlying DashMap doesn't require &mut self.
    #[test]
    fn test_delete_node_with_immutable_reference() {
        let storage = CurrentStorage::new();

        // Create a node
        let node_id = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();

        assert_eq!(storage.node_count(), 1);

        // Delete using immutable reference - this should compile and work
        // Thanks to DashMap's interior mutability
        let deleted = storage.delete_node(node_id).unwrap();
        assert_eq!(deleted.id, node_id);
        assert_eq!(storage.node_count(), 0);
    }

    /// Test that delete_edge can be called with an immutable reference.
    ///
    /// This test verifies that delete_edge(&self) works correctly since
    /// the underlying DashMap doesn't require &mut self.
    #[test]
    fn test_delete_edge_with_immutable_reference() {
        let storage = CurrentStorage::new();

        // Create two nodes and an edge
        let n0 = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let n1 = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();

        let edge_id = storage
            .create_edge(n0, n1, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();

        assert_eq!(storage.edge_count(), 1);

        // Delete using immutable reference - this should compile and work
        // Thanks to DashMap's interior mutability
        let deleted = storage.delete_edge(edge_id).unwrap();
        assert_eq!(deleted.id, edge_id);
        assert_eq!(storage.edge_count(), 0);
    }

    /// Test that delete operations can be called from a shared reference context.
    ///
    /// This test demonstrates a real-world scenario where we have a shared
    /// reference to storage and need to perform deletes.
    #[test]
    fn test_delete_operations_in_shared_context() {
        let storage = CurrentStorage::new();

        // Create test data
        let node1 = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let node2 = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let edge_id = storage
            .create_edge(node1, node2, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();

        assert_eq!(storage.node_count(), 2);
        assert_eq!(storage.edge_count(), 1);

        // Helper function that takes &CurrentStorage (shared reference)
        fn delete_data(storage: &CurrentStorage, node_id: NodeId, edge_id: EdgeId) {
            storage.delete_edge(edge_id).unwrap();
            storage.delete_node(node_id).unwrap();
        }

        // This should compile because delete methods accept &self
        delete_data(&storage, node1, edge_id);

        assert_eq!(storage.node_count(), 1);
        assert_eq!(storage.edge_count(), 0);
    }

    // ========================================================================
    // Tests for Issue #389: Multi-property Vector Index API
    // ========================================================================

    /// Test that multiple vector indexes can be enabled on different properties.
    ///
    /// This is the core multi-property feature: users should be able to index
    /// "title_embedding" and "body_embedding" simultaneously with different configs.
    #[test]
    fn test_enable_multiple_vector_indexes_on_different_properties() {
        use crate::index::vector::DistanceMetric;
        let storage = CurrentStorage::new();

        // Enable first index on "title_embedding" (384 dimensions)
        let config1 = HnswConfig::new(384, DistanceMetric::Cosine).with_capacity(100);
        storage
            .enable_vector_index("title_embedding", config1)
            .expect("Should enable first vector index");

        // Enable second index on "body_embedding" (1536 dimensions)
        let config2 = HnswConfig::new(1536, DistanceMetric::Cosine).with_capacity(100);
        storage
            .enable_vector_index("body_embedding", config2)
            .expect("Should enable second vector index on different property");

        // Both should be enabled
        assert!(storage.has_vector_index("title_embedding"));
        assert!(storage.has_vector_index("body_embedding"));
    }

    /// Test that re-enabling the same property fails.
    ///
    /// While different properties can have indexes, the same property
    /// cannot be indexed twice.
    #[test]
    fn test_enable_same_property_twice_fails() {
        use crate::index::vector::DistanceMetric;
        let storage = CurrentStorage::new();

        let config = HnswConfig::new(384, DistanceMetric::Cosine).with_capacity(100);
        storage
            .enable_vector_index("embedding", config.clone())
            .unwrap();

        // Second enable on SAME property should fail
        let result = storage.enable_vector_index("embedding", config);
        assert!(
            result.is_err(),
            "Should not allow re-enabling same property"
        );
    }

    /// Test find_similar_in to search a specific property's index.
    ///
    /// With multiple indexes, users need to specify which property to search.
    #[test]
    fn test_find_similar_in_specific_property() {
        use crate::index::vector::DistanceMetric;
        let storage = CurrentStorage::new();

        // Enable two indexes with different dimensions
        let config_title = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
        let config_body = HnswConfig::new(8, DistanceMetric::Cosine).with_capacity(100);
        storage
            .enable_vector_index("title_embedding", config_title)
            .unwrap();
        storage
            .enable_vector_index("body_embedding", config_body)
            .unwrap();

        // Create nodes with both properties
        let title_vec1 = vec![1.0f32, 0.0, 0.0, 0.0];
        let title_vec2 = vec![0.9f32, 0.1, 0.0, 0.0];
        let body_vec1 = vec![1.0f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let body_vec2 = vec![0.0f32, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]; // Different direction

        let node1 = storage
            .create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert_vector("title_embedding", &title_vec1)
                    .insert_vector("body_embedding", &body_vec1)
                    .build(),
            )
            .unwrap();
        let node2 = storage
            .create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert_vector("title_embedding", &title_vec2)
                    .insert_vector("body_embedding", &body_vec2)
                    .build(),
            )
            .unwrap();

        // Search title_embedding - node2 should be similar to node1
        let title_results = storage
            .find_similar_in("title_embedding", node1, 1)
            .unwrap();
        assert_eq!(title_results.len(), 1);
        assert_eq!(title_results[0].0, node2);

        // Search body_embedding - node2 is orthogonal to node1, so less similar
        let body_results = storage.find_similar_in("body_embedding", node1, 1).unwrap();
        assert_eq!(body_results.len(), 1);
        // Cosine similarity with orthogonal vectors should be ~0
        assert!(
            body_results[0].1 < 0.5,
            "Orthogonal vectors should have low similarity"
        );
    }

    /// Test that find_similar_in fails for non-indexed property.
    #[test]
    fn test_find_similar_in_property_not_indexed() {
        use crate::index::vector::DistanceMetric;
        let storage = CurrentStorage::new();

        let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
        storage
            .enable_vector_index("title_embedding", config)
            .unwrap();

        let vec1 = vec![1.0f32, 0.0, 0.0, 0.0];
        let node1 = storage
            .create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert_vector("title_embedding", &vec1)
                    .build(),
            )
            .unwrap();

        // Search on non-indexed property should fail
        let result = storage.find_similar_in("body_embedding", node1, 1);
        assert!(result.is_err(), "Should fail for non-indexed property");
    }

    /// Test list_vector_indexes returns all configured indexes.
    #[test]
    fn test_list_vector_indexes() {
        use crate::index::vector::DistanceMetric;
        let storage = CurrentStorage::new();

        // Initially empty
        let indexes = storage.list_vector_indexes();
        assert!(indexes.is_empty());

        // Add two indexes
        let config1 = HnswConfig::new(384, DistanceMetric::Cosine).with_capacity(100);
        let config2 = HnswConfig::new(1536, DistanceMetric::Euclidean).with_capacity(200);
        storage
            .enable_vector_index("title_embedding", config1)
            .unwrap();
        storage
            .enable_vector_index("body_embedding", config2)
            .unwrap();

        // Should list both
        let indexes = storage.list_vector_indexes();
        assert_eq!(indexes.len(), 2);

        let names: Vec<&str> = indexes.iter().map(|i| i.property_name.as_str()).collect();
        assert!(names.contains(&"title_embedding"));
        assert!(names.contains(&"body_embedding"));

        // Verify configs are preserved
        let title_idx = indexes
            .iter()
            .find(|i| i.property_name == "title_embedding")
            .unwrap();
        assert_eq!(title_idx.dimensions, 384);
        assert_eq!(title_idx.distance_metric, DistanceMetric::Cosine);

        let body_idx = indexes
            .iter()
            .find(|i| i.property_name == "body_embedding")
            .unwrap();
        assert_eq!(body_idx.dimensions, 1536);
        assert_eq!(body_idx.distance_metric, DistanceMetric::Euclidean);
    }

    /// Test has_vector_index for specific properties.
    #[test]
    fn test_has_vector_index_specific_property() {
        use crate::index::vector::DistanceMetric;
        let storage = CurrentStorage::new();

        assert!(!storage.has_vector_index("title_embedding"));
        assert!(!storage.has_vector_index("body_embedding"));

        let config = HnswConfig::new(384, DistanceMetric::Cosine).with_capacity(100);
        storage
            .enable_vector_index("title_embedding", config)
            .unwrap();

        assert!(storage.has_vector_index("title_embedding"));
        assert!(!storage.has_vector_index("body_embedding")); // Still not indexed
    }

    /// Test auto-indexing only indexes properties that have enabled indexes.
    #[test]
    fn test_auto_index_multiple_properties_selective() {
        use crate::index::vector::DistanceMetric;
        let storage = CurrentStorage::new();

        // Only enable index for title_embedding
        let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
        storage
            .enable_vector_index("title_embedding", config)
            .unwrap();

        // Create node with both properties
        let title_vec = vec![1.0f32, 0.0, 0.0, 0.0];
        let body_vec = vec![1.0f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let node1 = storage
            .create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert_vector("title_embedding", &title_vec)
                    .insert_vector("body_embedding", &body_vec)
                    .build(),
            )
            .unwrap();

        // Create another node for similarity search
        let title_vec2 = vec![0.9f32, 0.1, 0.0, 0.0];
        let _node2 = storage
            .create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert_vector("title_embedding", &title_vec2)
                    .build(),
            )
            .unwrap();

        // Search on title should work
        let results = storage
            .find_similar_in("title_embedding", node1, 1)
            .unwrap();
        assert_eq!(results.len(), 1);

        // Search on body should fail (not indexed)
        let result = storage.find_similar_in("body_embedding", node1, 1);
        assert!(result.is_err());
    }

    /// Test update_node updates the correct property index.
    #[test]
    fn test_update_node_updates_correct_property_index() {
        use crate::index::vector::DistanceMetric;
        let storage = CurrentStorage::new();

        // Enable both indexes
        let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
        storage
            .enable_vector_index("title_embedding", config.clone())
            .unwrap();
        storage
            .enable_vector_index("body_embedding", config)
            .unwrap();

        // Create nodes
        let v1 = vec![1.0f32, 0.0, 0.0, 0.0];
        let v2 = vec![0.0f32, 1.0, 0.0, 0.0];
        let node1 = storage
            .create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert_vector("title_embedding", &v1)
                    .insert_vector("body_embedding", &v1)
                    .build(),
            )
            .unwrap();
        let node2 = storage
            .create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert_vector("title_embedding", &v2)
                    .insert_vector("body_embedding", &v2)
                    .build(),
            )
            .unwrap();

        // Update node1's title_embedding to be similar to node2
        let v1_updated = vec![0.1f32, 0.9, 0.0, 0.0];
        let mut node1_obj = storage.get_node(node1).unwrap();
        node1_obj.properties = PropertyMapBuilder::new()
            .insert_vector("title_embedding", &v1_updated)
            .insert_vector("body_embedding", &v1) // Keep body the same
            .build();
        storage.update_node_direct(node1_obj, 2000.into()).unwrap();

        // Title search should now find node1 as similar to node2
        let title_results = storage
            .find_similar_in("title_embedding", node2, 1)
            .unwrap();
        assert_eq!(title_results[0].0, node1);

        // Body search should still find nodes dissimilar (orthogonal)
        let body_results = storage.find_similar_in("body_embedding", node2, 1).unwrap();
        // node1's body_embedding is still v1, orthogonal to v2
        assert!(body_results[0].1 < 0.5);
    }

    /// Test delete_node removes from all property indexes.
    #[test]
    fn test_delete_node_removes_from_all_indexes() {
        use crate::index::vector::DistanceMetric;
        let storage = CurrentStorage::new();

        // Enable two indexes
        let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
        storage
            .enable_vector_index("title_embedding", config.clone())
            .unwrap();
        storage
            .enable_vector_index("body_embedding", config)
            .unwrap();

        let v1 = vec![1.0f32, 0.0, 0.0, 0.0];
        let v2 = vec![0.9f32, 0.1, 0.0, 0.0];

        let node1 = storage
            .create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert_vector("title_embedding", &v1)
                    .insert_vector("body_embedding", &v1)
                    .build(),
            )
            .unwrap();
        let node2 = storage
            .create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert_vector("title_embedding", &v2)
                    .insert_vector("body_embedding", &v2)
                    .build(),
            )
            .unwrap();

        // Delete node2
        storage.delete_node_direct(node2, 3000.into()).unwrap();

        // Search from node1 should return empty in both indexes
        let title_results = storage
            .find_similar_in("title_embedding", node1, 2)
            .unwrap();
        assert_eq!(title_results.len(), 0);

        let body_results = storage.find_similar_in("body_embedding", node1, 2).unwrap();
        assert_eq!(body_results.len(), 0);
    }

    /// Test search_vectors_in for direct embedding queries.
    #[test]
    fn test_search_vectors_in_by_embedding() {
        use crate::index::vector::DistanceMetric;
        let storage = CurrentStorage::new();

        let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
        storage.enable_vector_index("embedding", config).unwrap();

        let v1 = vec![1.0f32, 0.0, 0.0, 0.0];
        let v2 = vec![0.9f32, 0.1, 0.0, 0.0];
        let v3 = vec![0.0f32, 1.0, 0.0, 0.0];

        let node1 = storage
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &v1)
                    .build(),
            )
            .unwrap();
        let node2 = storage
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &v2)
                    .build(),
            )
            .unwrap();
        let _node3 = storage
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &v3)
                    .build(),
            )
            .unwrap();

        // Search with direct embedding
        let query = vec![1.0f32, 0.0, 0.0, 0.0];
        let results = storage.search_vectors_in("embedding", &query, 2).unwrap();
        assert_eq!(results.len(), 2);
        // v1 is identical to query, so node1 is most similar
        assert_eq!(results[0].0, node1);
        // v2 is second most similar (close to [1,0,0,0])
        assert_eq!(results[1].0, node2);
    }

    /// Test dimension mismatch fails for specific property.
    #[test]
    fn test_dimension_mismatch_specific_property() {
        use crate::index::vector::DistanceMetric;
        let storage = CurrentStorage::new();

        // Title: 4D, Body: 8D
        let config_title = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
        let config_body = HnswConfig::new(8, DistanceMetric::Cosine).with_capacity(100);
        storage
            .enable_vector_index("title_embedding", config_title)
            .unwrap();
        storage
            .enable_vector_index("body_embedding", config_body)
            .unwrap();

        // Wrong dimension for title (8D instead of 4D)
        let wrong_title = vec![1.0f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let correct_body = vec![1.0f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];

        let result = storage.create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert_vector("title_embedding", &wrong_title)
                .insert_vector("body_embedding", &correct_body)
                .build(),
        );

        assert!(result.is_err(), "Should fail on dimension mismatch");
    }

    // TDD tests for Issue #187 - Performance optimization for graph traversal methods
    // These tests verify the new iterator-based API that avoids unnecessary Vec allocations

    #[test]
    fn test_get_outgoing_edges_iter_basic() {
        let storage = CurrentStorage::new();

        // Create nodes
        let n0 = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let n1 = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let n2 = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();

        // Create edges
        let e1 = storage
            .create_edge(n0, n1, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();
        let e2 = storage
            .create_edge(n0, n2, "FOLLOWS", PropertyMapBuilder::new().build())
            .unwrap();

        // Test iterator returns same results as Vec version using HashSet for robust comparison
        let vec_result: std::collections::HashSet<EdgeId> =
            storage.get_outgoing_edges(n0).into_iter().collect();
        let iter_result: std::collections::HashSet<EdgeId> =
            storage.get_outgoing_edges_iter(n0).collect();

        assert_eq!(vec_result, iter_result);
        assert!(iter_result.contains(&e1));
        assert!(iter_result.contains(&e2));
    }

    #[test]
    fn test_get_outgoing_edges_iter_empty() {
        let storage = CurrentStorage::new();

        let n0 = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();

        // Node with no outgoing edges
        let count = storage.get_outgoing_edges_iter(n0).count();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_get_incoming_edges_iter_basic() {
        let storage = CurrentStorage::new();

        // Create nodes
        let n0 = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let n1 = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let n2 = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();

        // Create edges pointing to n2
        let e1 = storage
            .create_edge(n0, n2, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();
        let e2 = storage
            .create_edge(n1, n2, "FOLLOWS", PropertyMapBuilder::new().build())
            .unwrap();

        // Test iterator returns same results as Vec version using HashSet for robust comparison
        let vec_result: std::collections::HashSet<EdgeId> =
            storage.get_incoming_edges(n2).into_iter().collect();
        let iter_result: std::collections::HashSet<EdgeId> =
            storage.get_incoming_edges_iter(n2).collect();

        assert_eq!(vec_result, iter_result);
        assert!(iter_result.contains(&e1));
        assert!(iter_result.contains(&e2));
    }

    #[test]
    fn test_get_incoming_edges_iter_empty() {
        let storage = CurrentStorage::new();

        let n0 = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();

        // Node with no incoming edges
        let count = storage.get_incoming_edges_iter(n0).count();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_get_outgoing_edges_with_label_iter_basic() {
        let storage = CurrentStorage::new();

        let n0 = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let n1 = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let n2 = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();

        // Create edges with different labels
        let e1 = storage
            .create_edge(n0, n1, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();
        let _e2 = storage
            .create_edge(n0, n2, "FOLLOWS", PropertyMapBuilder::new().build())
            .unwrap();

        // Test iterator with label filter
        let iter_result: Vec<EdgeId> = storage
            .get_outgoing_edges_with_label_iter(n0, "KNOWS")
            .collect();
        assert_eq!(iter_result.len(), 1);
        assert!(iter_result.contains(&e1));

        // Compare with Vec version
        let vec_result = storage.get_outgoing_edges_with_label(n0, "KNOWS");
        assert_eq!(vec_result.len(), iter_result.len());
    }

    #[test]
    fn test_get_outgoing_edges_with_label_iter_nonexistent_label() {
        let storage = CurrentStorage::new();

        let n0 = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let n1 = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();

        storage
            .create_edge(n0, n1, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();

        // Non-existent label should return empty iterator
        let count = storage
            .get_outgoing_edges_with_label_iter(n0, "LOVES")
            .count();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_get_incoming_edges_with_label_iter_basic() {
        let storage = CurrentStorage::new();

        let n0 = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let n1 = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let n2 = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();

        // Create edges with different labels pointing to n2
        let e1 = storage
            .create_edge(n0, n2, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();
        let _e2 = storage
            .create_edge(n1, n2, "FOLLOWS", PropertyMapBuilder::new().build())
            .unwrap();

        // Test iterator with label filter
        let iter_result: Vec<EdgeId> = storage
            .get_incoming_edges_with_label_iter(n2, "KNOWS")
            .collect();
        assert_eq!(iter_result.len(), 1);
        assert!(iter_result.contains(&e1));

        // Compare with Vec version
        let vec_result = storage.get_incoming_edges_with_label(n2, "KNOWS");
        assert_eq!(vec_result.len(), iter_result.len());
    }

    #[test]
    fn test_get_incoming_edges_with_label_iter_nonexistent_label() {
        let storage = CurrentStorage::new();

        let n0 = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let n1 = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();

        storage
            .create_edge(n0, n1, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();

        // Non-existent label should return empty iterator
        let count = storage
            .get_incoming_edges_with_label_iter(n1, "LOVES")
            .count();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_iterator_can_be_partially_consumed() {
        let storage = CurrentStorage::new();

        let n0 = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let n1 = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let n2 = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let n3 = storage
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();

        // Create multiple edges
        storage
            .create_edge(n0, n1, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();
        storage
            .create_edge(n0, n2, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();
        storage
            .create_edge(n0, n3, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();

        // Take only first 2 elements - demonstrating lazy evaluation benefit
        let first_two: Vec<EdgeId> = storage.get_outgoing_edges_iter(n0).take(2).collect();
        assert_eq!(first_two.len(), 2);
    }

    #[test]
    fn test_iterator_consistency_with_vec() {
        let storage = CurrentStorage::new();

        // Create a more complex graph
        let nodes: Vec<NodeId> = (0..5)
            .map(|_| {
                storage
                    .create_node("Person", PropertyMapBuilder::new().build())
                    .unwrap()
            })
            .collect();

        // Create a star pattern: n0 -> n1, n2, n3, n4
        for i in 1..5 {
            storage
                .create_edge(
                    nodes[0],
                    nodes[i],
                    "KNOWS",
                    PropertyMapBuilder::new().build(),
                )
                .unwrap();
        }

        // Verify iterator and Vec return same edges (order may differ)
        let vec_edges: std::collections::HashSet<EdgeId> =
            storage.get_outgoing_edges(nodes[0]).into_iter().collect();
        let iter_edges: std::collections::HashSet<EdgeId> =
            storage.get_outgoing_edges_iter(nodes[0]).collect();

        assert_eq!(vec_edges, iter_edges);
    }
}
