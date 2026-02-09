//! HNSW (Hierarchical Navigable Small World) vector index implementation.
//!
//! This module provides a wrapper around the `usearch` library's HNSW index,
//! implementing the `VectorIndex` trait for approximate k-nearest neighbor search.
//!
//! # Overview
//!
//! HNSW is a graph-based algorithm for approximate nearest neighbor search that
//! provides excellent search performance with logarithmic average-case complexity:
//!
//! - **Build time**: O(n log n) average case
//! - **Query time**: O(log n) average case, O(n) worst case
//! - **Memory**: O(n * M * d) where M is connections per node, d is dimensions
//!
//! # Performance Characteristics
//!
//! Based on usearch benchmarks and AletheiaDB testing:
//! - **Add operation**: 1-10us per vector (depends on M, ef_construction)
//! - **Search operation**: 100us-1ms for k=10 (depends on index size, ef_search, dimensions)
//! - **Memory usage**: ~(dimensions + M) * 4 bytes per vector (less with quantization)
//!
//! # Features
//!
//! - **Native deletes**: Vectors are truly removed from the index
//! - **Quantization**: F32 (full), F16 (half), I8 (quarter precision)
//! - **Memory-mapped indexes**: Serve large indexes from disk (read-only)
//! - **Custom distance metrics**: User-defined similarity functions
//! - **New distance metrics**: Haversine, Hamming, Tanimoto
//!
//! # Tuning Parameters
//!
//! ## M (connections per node)
//! - **Range**: 8-64
//! - **Lower values**: Less memory, faster build, lower recall
//! - **Higher values**: More memory, slower build, higher recall
//! - **Default**: 16 (good balance for most use cases)
//!
//! ## ef_construction (build-time expansion)
//! - **Range**: 100-500
//! - **Higher values**: Better index quality, slower build time
//! - **Default**: 128
//!
//! ## ef_search (query-time expansion)
//! - **Range**: 10-500
//! - **Higher values**: Better recall, slower queries
//! - **Default**: 64
//! - **Can be adjusted at runtime** via `set_ef_search()` method
//!
//! # Examples
//!
//! ```rust,no_run
//! use aletheiadb::index::vector::{HnswIndexBuilder, DistanceMetric, Quantization};
//! use aletheiadb::index::VectorIndex;
//! use aletheiadb::core::id::NodeId;
//!
//! # fn example() -> aletheiadb::utils::Result<()> {
//! // Create an index for 384-dimensional embeddings using cosine similarity
//! let index = HnswIndexBuilder::new(384, DistanceMetric::Cosine)
//!     .m(16)                    // 16 connections per node
//!     .ef_construction(200)     // Build quality
//!     .quantization(Quantization::F16)  // Half precision for memory savings
//!     .initial_capacity(10000)  // Pre-allocate for 10k vectors
//!     .build()?;
//!
//! // Add vectors
//! let node1 = NodeId::new(1).unwrap();
//! let embedding1 = vec![0.1f32; 384];
//! index.add(node1, &embedding1)?;
//!
//! // Search for similar vectors
//! let query = vec![0.15f32; 384];
//! let results = index.search(&query, 10)?;
//!
//! for (node_id, similarity) in results {
//!     println!("Found node {:?} with similarity {}", node_id, similarity);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # Thread Safety
//!
//! `HnswIndex` is fully thread-safe for concurrent operations:
//! - Multiple threads can add vectors simultaneously
//! - Multiple threads can search simultaneously
//! - Searches can run concurrently with additions

use crate::core::id::NodeId;
use crate::core::vector::validate_vector;
use crate::index::vector::{CustomMetric, DistanceMetric, Quantization, StorageMode, VectorIndex};
use crate::utils::{Error, Result, error::VectorError};
use crc32fast::Hasher;
use dashmap::DashMap;
use parking_lot::RwLock;
use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use usearch::{Index, IndexOptions, MetricKind, ScalarKind, ffi::Matches};

// Thread-local flag to detect re-entrant modification attempts during filtered search.
// This prevents deadlocks when user filter callbacks try to modify the index.
std::thread_local! {
    static IN_FILTER_CALLBACK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// RAII guard that sets IN_FILTER_CALLBACK to true on creation and false on drop.
/// This ensures the flag is always reset, even if the callback panics.
struct FilterCallbackGuard;

impl FilterCallbackGuard {
    fn new() -> Self {
        IN_FILTER_CALLBACK.with(|flag| flag.set(true));
        FilterCallbackGuard
    }
}

impl Drop for FilterCallbackGuard {
    fn drop(&mut self) {
        IN_FILTER_CALLBACK.with(|flag| flag.set(false));
    }
}

/// Magic bytes for mapping file identification
const MAPPING_MAGIC: &[u8; 4] = b"GMAP";
/// Current mapping file format version
const MAPPING_VERSION: u8 = 1;

/// Maximum number of results that can be requested in a search.
///
/// Increased from 10K to 100K to support business scenarios:
/// - Bulk similarity computations and exports
/// - Large-scale batch processing
/// - Migration and data analysis operations
///
///   This prevents DoS attacks via excessive memory allocation while enabling
///   legitimate bulk operations.
const MAX_K: usize = 100_000;

/// Convert our DistanceMetric to usearch's MetricKind
fn to_usearch_metric(metric: DistanceMetric) -> MetricKind {
    match metric {
        DistanceMetric::Cosine => MetricKind::Cos,
        DistanceMetric::Euclidean => MetricKind::L2sq,
        DistanceMetric::DotProduct => MetricKind::IP,
        DistanceMetric::Haversine => MetricKind::Haversine,
        DistanceMetric::Hamming => MetricKind::Hamming,
        DistanceMetric::Tanimoto => MetricKind::Tanimoto,
    }
}

/// Convert our Quantization to usearch's ScalarKind
fn to_usearch_scalar(quantization: Quantization) -> ScalarKind {
    match quantization {
        Quantization::F32 => ScalarKind::F32,
        Quantization::F16 => ScalarKind::F16,
        Quantization::I8 => ScalarKind::I8,
    }
}

/// Configuration for HNSW (Hierarchical Navigable Small World) index.
///
/// This struct encapsulates all parameters needed to configure an HNSW index
/// for approximate nearest neighbor search. It provides sensible defaults
/// optimized for a balance between accuracy, speed, and memory usage.
#[derive(Debug, Clone)]
pub struct HnswConfig {
    /// Vector dimensionality (must be > 0)
    pub dimensions: usize,
    /// Distance metric for similarity computation
    pub metric: DistanceMetric,
    /// Maximum bidirectional connections per node (default: 16)
    pub m: usize,
    /// Build-time candidate list size (default: 128)
    pub ef_construction: usize,
    /// Query-time candidate list size (default: 64)
    pub ef_search: usize,
    /// Initial capacity for pre-allocation (default: 0)
    pub capacity: usize,
    /// Quantization level (default: F32)
    pub quantization: Quantization,
    /// Storage mode (default: InMemory)
    pub storage: StorageMode,
    /// Custom distance metric (overrides `metric` if set)
    pub custom_metric: Option<CustomMetric>,
}

impl PartialEq for HnswConfig {
    fn eq(&self, other: &Self) -> bool {
        self.dimensions == other.dimensions
            && self.metric == other.metric
            && self.m == other.m
            && self.ef_construction == other.ef_construction
            && self.ef_search == other.ef_search
            && self.capacity == other.capacity
            && self.quantization == other.quantization
            && self.storage == other.storage
            && self.custom_metric == other.custom_metric
    }
}

impl Default for HnswConfig {
    fn default() -> Self {
        HnswConfig {
            dimensions: 0,
            metric: DistanceMetric::Cosine,
            m: 16,
            ef_construction: 128,
            ef_search: 64,
            capacity: 0,
            quantization: Quantization::default(),
            storage: StorageMode::default(),
            custom_metric: None,
        }
    }
}

impl HnswConfig {
    /// Creates a new configuration with the specified dimensions and metric.
    pub fn new(dimensions: usize, metric: DistanceMetric) -> Self {
        HnswConfig {
            dimensions,
            metric,
            ..Default::default()
        }
    }

    /// Sets the M parameter (connections per node).
    pub fn with_m(mut self, m: usize) -> Self {
        self.m = m;
        self
    }

    /// Sets ef_construction (build-time expansion).
    pub fn with_ef_construction(mut self, ef_construction: usize) -> Self {
        self.ef_construction = ef_construction;
        self
    }

    /// Sets ef_search (query-time expansion).
    pub fn with_ef_search(mut self, ef_search: usize) -> Self {
        self.ef_search = ef_search;
        self
    }

    /// Sets initial capacity.
    pub fn with_capacity(mut self, capacity: usize) -> Self {
        self.capacity = capacity;
        self
    }

    /// Sets the dimensions.
    pub fn with_dimensions(mut self, dimensions: usize) -> Self {
        self.dimensions = dimensions;
        self
    }

    /// Sets the distance metric.
    pub fn with_metric(mut self, metric: DistanceMetric) -> Self {
        self.metric = metric;
        self
    }

    /// Sets the quantization level.
    pub fn with_quantization(mut self, quantization: Quantization) -> Self {
        self.quantization = quantization;
        self
    }

    /// Sets the storage mode.
    pub fn with_storage(mut self, storage: StorageMode) -> Self {
        self.storage = storage;
        self
    }

    /// Sets a custom distance metric function.
    pub fn with_custom_metric<F>(mut self, name: &str, f: F) -> Self
    where
        F: Fn(&[f32], &[f32]) -> f32 + Send + Sync + 'static,
    {
        self.custom_metric = Some(CustomMetric {
            name: name.to_string(),
            distance_fn: Arc::new(f),
        });
        self
    }

    /// Serialize configuration to a writer in little-endian binary format.
    pub fn serialize_into<W: Write>(&self, writer: &mut W) -> Result<()> {
        writer.write_all(&(self.dimensions as u64).to_le_bytes())?;
        writer.write_all(&[self.metric.to_u8()])?;
        writer.write_all(&(self.m as u64).to_le_bytes())?;
        writer.write_all(&(self.ef_construction as u64).to_le_bytes())?;
        writer.write_all(&(self.ef_search as u64).to_le_bytes())?;
        writer.write_all(&(self.capacity as u64).to_le_bytes())?;
        Ok(())
    }

    /// Deserialize configuration from a reader.
    pub fn deserialize_from<R: Read>(reader: &mut R) -> Result<Self> {
        let mut buf_u64 = [0u8; 8];
        let mut buf_u8 = [0u8; 1];

        reader.read_exact(&mut buf_u64)?;
        let dimensions = u64::from_le_bytes(buf_u64) as usize;

        reader.read_exact(&mut buf_u8)?;
        let metric = DistanceMetric::from_u8(buf_u8[0])?;

        reader.read_exact(&mut buf_u64)?;
        let m = u64::from_le_bytes(buf_u64) as usize;

        reader.read_exact(&mut buf_u64)?;
        let ef_construction = u64::from_le_bytes(buf_u64) as usize;

        reader.read_exact(&mut buf_u64)?;
        let ef_search = u64::from_le_bytes(buf_u64) as usize;

        reader.read_exact(&mut buf_u64)?;
        let capacity = u64::from_le_bytes(buf_u64) as usize;

        Ok(HnswConfig {
            dimensions,
            metric,
            m,
            ef_construction,
            ef_search,
            capacity,
            ..Default::default()
        })
    }
}

/// Statistics for index operations.
#[derive(Debug, Default)]
struct IndexStats {
    vectors_added: AtomicU64,
    vectors_removed: AtomicU64,
    searches_performed: AtomicU64,
    /// Number of times search operations were retried due to transient errors
    search_retries: AtomicU64,
    /// Number of searches that failed even after all retry attempts
    search_retry_failures: AtomicU64,
}

/// Maximum number of search attempts (initial attempt + retries) when encountering transient errors.
///
/// Under high concurrent load, usearch may fail with "No available threads to lock" when its
/// internal thread pool is exhausted. This constant controls how many times we retry before
/// giving up.
///
/// # Performance Impact
///
/// With exponential backoff (1ms, 2ms, 4ms), a query that exhausts all retry attempts will
/// add up to 7ms latency. This is significant relative to the project's performance targets:
/// - k-NN search target: <10ms
/// - Hybrid query target: <30ms
///
/// Operators should monitor `search_retries` and `search_retry_failures` metrics. Frequent
/// retries indicate thread pool exhaustion and may require tuning usearch parameters or
/// reducing concurrency.
const MAX_SEARCH_ATTEMPTS: u32 = 4; // 1 initial attempt + 3 retries

/// Check if a usearch error is transient and should be retried.
///
/// # Warning: Fragile Implementation
///
/// This function relies on string matching against usearch error messages. If usearch changes
/// its error messages in future versions, this detection may break silently. Callers should
/// monitor retry metrics to detect if retries stop working.
///
/// # Known Retryable Errors
///
/// - "No available threads to lock": Thread pool exhaustion under high concurrency
///
/// # Arguments
///
/// * `error_msg` - The error message string from usearch
///
/// # Returns
///
/// `true` if the error is transient and safe to retry, `false` otherwise
#[inline]
fn is_retryable_usearch_error(error_msg: &str) -> bool {
    // Thread pool exhaustion is a transient error that resolves when threads become available
    error_msg.contains("No available threads to lock")
}

// Helper to create the metric wrapper - extracted for testing
fn create_metric_wrapper<F>(
    dims: usize,
    distance_fn: Arc<F>,
) -> Box<dyn Fn(*const f32, *const f32) -> f32 + Send + Sync>
where
    F: Fn(&[f32], &[f32]) -> f32 + Send + Sync + 'static + ?Sized,
{
    Box::new(move |a: *const f32, b: *const f32| {
        // Check for null pointers to prevent UB
        if a.is_null() || b.is_null() {
            // This should never happen with a correct usearch implementation.
            // If it does, we panic to prevent UB from dereferencing null.
            // We cannot return an error here because the signature is fixed by usearch trait.
            panic!("usearch passed null pointer to metric function");
        }

        // Check for alignment to prevent UB
        // Use bitwise check for power-of-2 alignment (f32 align is 4)
        let align_mask = std::mem::align_of::<f32>() - 1;
        if (a as usize) & align_mask != 0 || (b as usize) & align_mask != 0 {
            panic!(
                "usearch passed unaligned pointer to metric function (expected alignment {})",
                std::mem::align_of::<f32>()
            );
        }

        // SAFETY: usearch guarantees pointers are valid for `dims` elements.
        // We verified they are not null above.

        // Strict alignment check to prevent UB (Sentry Directive)
        // f32 requires 4-byte alignment. accessing unaligned data via slice is UB.
        if a.align_offset(std::mem::align_of::<f32>()) != 0
            || b.align_offset(std::mem::align_of::<f32>()) != 0
        {
            panic!("usearch passed unaligned pointer to metric function");
        }

        let slice_a = unsafe { std::slice::from_raw_parts(a, dims) };
        let slice_b = unsafe { std::slice::from_raw_parts(b, dims) };
        distance_fn(slice_a, slice_b)
    })
}

/// Builder for configuring and creating an `HnswIndex`.
pub struct HnswIndexBuilder {
    config: HnswConfig,
}

impl HnswIndexBuilder {
    /// Creates a new builder with the required parameters.
    pub fn new(dimensions: usize, metric: DistanceMetric) -> Self {
        HnswIndexBuilder {
            config: HnswConfig {
                dimensions,
                metric,
                ..Default::default()
            },
        }
    }

    /// Creates a builder from an existing configuration.
    pub fn from_config(config: &HnswConfig) -> Self {
        HnswIndexBuilder {
            config: config.clone(),
        }
    }

    /// Sets the M parameter (connections per node).
    pub fn m(mut self, m: usize) -> Self {
        self.config.m = m;
        self
    }

    /// Sets ef_construction (build-time expansion).
    pub fn ef_construction(mut self, ef_construction: usize) -> Self {
        self.config.ef_construction = ef_construction;
        self
    }

    /// Sets ef_search (query-time expansion).
    pub fn ef_search(mut self, ef_search: usize) -> Self {
        self.config.ef_search = ef_search;
        self
    }

    /// Sets initial capacity hint for pre-allocation.
    pub fn initial_capacity(mut self, capacity: usize) -> Self {
        self.config.capacity = capacity;
        self
    }

    /// Sets quantization level.
    pub fn quantization(mut self, quantization: Quantization) -> Self {
        self.config.quantization = quantization;
        self
    }

    /// Sets storage mode.
    pub fn storage(mut self, storage: StorageMode) -> Self {
        self.config.storage = storage;
        self
    }

    /// Sets a custom distance metric function.
    pub fn with_custom_metric<F>(mut self, name: &str, f: F) -> Self
    where
        F: Fn(&[f32], &[f32]) -> f32 + Send + Sync + 'static,
    {
        self.config = self.config.with_custom_metric(name, f);
        self
    }

    /// Builds the HNSW index with the configured parameters.
    pub fn build(self) -> Result<HnswIndex> {
        // Validate dimensions
        if self.config.dimensions == 0 {
            return Err(Error::Vector(VectorError::InvalidVector {
                reason: "dimensions must be > 0".to_string(),
            }));
        }

        // Validate M
        if self.config.m == 0 || self.config.m > 64 {
            return Err(Error::Vector(VectorError::InvalidVector {
                reason: format!("M must be in range [1, 64], got {}", self.config.m),
            }));
        }

        // Validate ef_construction
        // Prevent DoS via excessive memory allocation
        if self.config.ef_construction < 10 || self.config.ef_construction > 4096 {
            return Err(Error::Vector(VectorError::InvalidVector {
                reason: format!(
                    "ef_construction must be in range [10, 4096], got {}",
                    self.config.ef_construction
                ),
            }));
        }

        // Validate ef_search
        // Prevent DoS via excessive CPU/Memory usage
        if self.config.ef_search < 1 || self.config.ef_search > 4096 {
            return Err(Error::Vector(VectorError::InvalidVector {
                reason: format!(
                    "ef_search must be in range [1, 4096], got {}",
                    self.config.ef_search
                ),
            }));
        }

        // Security Check: Custom metrics require F32 quantization
        // This is critical because usearch passes raw pointers to the metric function.
        // If quantization is not F32 (e.g., I8 or F16), the pointers will point to
        // compressed data, but our metric wrapper casts them to `*const f32`.
        // This would cause a buffer over-read (reading 4x or 2x memory), leading to
        // potential crashes (DoS) or information leakage.
        if self.config.custom_metric.is_some() && self.config.quantization != Quantization::F32 {
            return Err(Error::Vector(VectorError::InvalidVector {
                reason: format!(
                    "Custom metrics are only supported with F32 quantization (requested {:?}). \
                     Using other quantization levels with custom metrics causes memory safety issues.",
                    self.config.quantization
                ),
            }));
        }

        // Create usearch index options
        let options = IndexOptions {
            dimensions: self.config.dimensions,
            metric: to_usearch_metric(self.config.metric),
            quantization: to_usearch_scalar(self.config.quantization),
            connectivity: self.config.m,
            expansion_add: self.config.ef_construction,
            expansion_search: self.config.ef_search,
            multi: false,
        };

        // Create the index
        let mut index = Index::new(&options).map_err(|e| {
            Error::Vector(VectorError::IndexError(format!(
                "Failed to create usearch index: {}",
                e
            )))
        })?;

        // Reserve capacity - usearch requires capacity before adding vectors
        // Use configured capacity, or default to 1024 for reasonable initial size
        let capacity_to_reserve = if self.config.capacity > 0 {
            self.config.capacity
        } else {
            1024 // Reasonable default for initial capacity
        };
        index.reserve(capacity_to_reserve).map_err(|e| {
            Error::Vector(VectorError::IndexError(format!(
                "Failed to reserve capacity: {}",
                e
            )))
        })?;

        // Apply custom metric if configured
        if let Some(ref custom) = self.config.custom_metric {
            let dims = self.config.dimensions;
            let distance_fn = Arc::clone(&custom.distance_fn);

            // Create a wrapper that converts usearch's raw pointer API to our safe slice API
            // SAFETY: usearch guarantees that:
            // 1. Both pointers are valid and point to `dims` contiguous f32 values
            // 2. The pointers remain valid for the duration of the function call
            // 3. The data is properly aligned for f32
            let metric_wrapper = create_metric_wrapper(dims, distance_fn);

            index.change_metric(metric_wrapper);
        }

        // Handle memory-mapped storage
        if let StorageMode::MemoryMapped { ref path } = self.config.storage {
            // Save initial empty index to create the file
            index
                .save(path.to_str().ok_or_else(|| {
                    Error::Vector(VectorError::IndexError(
                        "Path contains invalid UTF-8".to_string(),
                    ))
                })?)
                .map_err(|e| {
                    Error::Vector(VectorError::IndexError(format!(
                        "Failed to create memory-mapped index: {}",
                        e
                    )))
                })?;
            // Switch to view mode (memory-mapped)
            index
                .view(path.to_str().ok_or_else(|| {
                    Error::Vector(VectorError::IndexError(
                        "Path contains invalid UTF-8".to_string(),
                    ))
                })?)
                .map_err(|e| {
                    Error::Vector(VectorError::IndexError(format!(
                        "Failed to memory-map index: {}",
                        e
                    )))
                })?;
        }

        Ok(HnswIndex {
            inner: Arc::new(RwLock::new(index)),
            config: self.config,
            id_mapping: Arc::new(DashMap::new()),
            reverse_mapping: Arc::new(DashMap::new()),
            next_key: AtomicU64::new(0),
            stats: Arc::new(IndexStats::default()),
            max_k: MAX_K,
            is_mmap: false,
        })
    }
}

/// HNSW vector index for approximate k-nearest neighbor search.
///
/// This struct wraps `usearch::Index` and implements the `VectorIndex` trait.
/// All operations are thread-safe.
///
/// # Native Deletes
///
/// Unlike the previous hnsw_rs implementation, usearch supports native deletes.
/// Removed vectors are truly removed from the index, not just soft-deleted.
///
/// # Memory-Mapped Indexes
///
/// Indexes opened via `open_mmap()` are read-only. Attempting to call `add()`
/// or `remove()` on a memory-mapped index will return an error.
pub struct HnswIndex {
    /// Underlying usearch index
    inner: Arc<RwLock<Index>>,
    /// Configuration used to create this index
    config: HnswConfig,
    /// ID mapping: NodeId -> usearch key (u64)
    id_mapping: Arc<DashMap<NodeId, u64>>,
    /// Reverse mapping: usearch key -> NodeId
    reverse_mapping: Arc<DashMap<u64, NodeId>>,
    /// Next available key
    next_key: AtomicU64,
    /// Statistics
    stats: Arc<IndexStats>,
    /// Maximum k for DoS protection
    max_k: usize,
    /// Whether this index is memory-mapped (read-only)
    is_mmap: bool,
}

// ╔═══════════════════════════════════════════════════════════════════════════╗
// ║ LOCK ORDERING INVARIANT (Deadlock Prevention - PR #751)                  ║
// ╠═══════════════════════════════════════════════════════════════════════════╣
// ║ ALL operations MUST acquire locks in this order:                         ║
// ║   1. inner (RwLock<Index>)           - FIRST                             ║
// ║   2. id_mapping/reverse_mapping (DashMap) - SECOND                       ║
// ║                                                                           ║
// ║ NEVER hold DashMap shard locks while acquiring inner lock.               ║
// ║ Violating this order causes deadlock between add() and save().           ║
// ╚═══════════════════════════════════════════════════════════════════════════╝
impl std::fmt::Debug for HnswIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HnswIndex")
            .field("config", &self.config)
            .field("is_mmap", &self.is_mmap)
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

impl VectorIndex for HnswIndex {
    fn add(&self, id: NodeId, vector: &[f32]) -> Result<()> {
        // Check for re-entrant modification during filtered search (prevents deadlock)
        if IN_FILTER_CALLBACK.with(|flag| flag.get()) {
            return Err(Error::Vector(VectorError::IndexError(
                "Cannot modify index from within a search_with_filter callback. \
                 This would cause a deadlock due to lock re-entrancy. \
                 Consider collecting modifications and applying them after the search completes."
                    .to_string(),
            )));
        }

        // Check if index is read-only (memory-mapped)
        if self.is_mmap {
            return Err(Error::Vector(VectorError::IndexError(
                "Cannot modify memory-mapped index (read-only)".to_string(),
            )));
        }

        // Validate vector
        validate_vector(vector)?;

        // Check dimensions match
        if vector.len() != self.config.dimensions {
            return Err(Error::Vector(VectorError::DimensionMismatch {
                expected: self.config.dimensions,
                actual: vector.len(),
            }));
        }

        // LOCK ORDERING FIX: We must always acquire inner (RwLock) BEFORE id_mapping (DashMap).
        // This prevents deadlocks with save() which now also follows this order (Inner -> Mappings).
        //
        // Previous Vacant path acquired Inner -> Dropped Inner -> Acquired DM.
        // This gap allowed save() to sneak in and capture a Phantom Vector.
        //
        // New logic uses an optimistic retry loop to strictly follow: Lock Inner -> Lock/Check DM.

        loop {
            // Path 1: Occupied (Optimistic check)
            if let Some(existing_key) = self.id_mapping.get(&id).map(|k| *k) {
                // Acquire inner lock FIRST
                let index = self.inner.write();

                // Re-verify mapping while holding inner lock
                // This prevents race where another thread removed/updated the ID
                // Note: We cannot collapse this if due to unstable let_chains
                #[allow(clippy::collapsible_if)]
                if let Some(current_key) = self.id_mapping.get(&id).map(|k| *k) {
                    if current_key == existing_key {
                        // Confirmed: Update existing node
                        // Optimization (Issue #207): Only call remove() if key actually exists in usearch.
                        if index.contains(existing_key) {
                            index.remove(existing_key).map_err(|e| {
                                Error::Vector(VectorError::IndexError(format!(
                                    "Failed to remove existing vector: {}",
                                    e
                                )))
                            })?;
                        }

                        // Check capacity
                        if index.size() >= index.capacity() {
                            let new_capacity = (index.capacity() * 2).max(1024);
                            index.reserve(new_capacity).map_err(|e| {
                                Error::Vector(VectorError::IndexError(format!(
                                    "Failed to expand capacity: {}",
                                    e
                                )))
                            })?;
                        }

                        // Add new vector
                        index.add(existing_key, vector).map_err(|e| {
                            Error::Vector(VectorError::IndexError(format!(
                                "Failed to add vector: {}",
                                e
                            )))
                        })?;

                        self.stats.vectors_added.fetch_add(1, Ordering::Relaxed);
                        return Ok(());
                    }
                }
                // Mapping changed/removed while we waited for lock. Retry.
                continue;
            }

            // Path 2: Vacant (New Node)
            const MAX_VALID_KEY: u64 = u64::MAX - 1000;

            // Acquire inner lock FIRST
            let index = self.inner.write();

            // Check capacity
            if index.size() >= index.capacity() {
                let new_capacity = (index.capacity() * 2).max(1024);
                index.reserve(new_capacity).map_err(|e| {
                    Error::Vector(VectorError::IndexError(format!(
                        "Failed to expand capacity: {}",
                        e
                    )))
                })?;
            }

            // Now acquire DashMap lock via entry API
            // Safe because we hold Inner (Write), blocking other adds/saves.
            // Save/Search hold Inner (Read), so they can't deadlock against us.
            match self.id_mapping.entry(id) {
                dashmap::mapref::entry::Entry::Occupied(_) => {
                    // Race: Someone added it while we were preparing. Retry.
                    // Implicitly drops entry lock, then inner lock.
                    continue;
                }
                dashmap::mapref::entry::Entry::Vacant(e) => {
                    // ALLOCATE KEY HERE (inside lock scope) to prevent gaps/exhaustion
                    // If we allocated outside, and then hit the 'Occupied' race above,
                    // we'd leak a key index.
                    let key = loop {
                        let current = self.next_key.load(Ordering::SeqCst);
                        if current > MAX_VALID_KEY {
                            return Err(Error::Vector(VectorError::IndexError(
                                "Maximum number of vectors exceeded (key overflow protection)"
                                    .to_string(),
                            )));
                        }
                        match self.next_key.compare_exchange(
                            current,
                            current + 1,
                            Ordering::SeqCst,
                            Ordering::SeqCst,
                        ) {
                            Ok(key) => break key,
                            Err(_) => continue,
                        }
                    };

                    // Add to inner usearch index
                    index.add(key, vector).map_err(|e| {
                        Error::Vector(VectorError::IndexError(format!(
                            "Failed to add vector: {}",
                            e
                        )))
                    })?;

                    // Add to mappings (while holding inner lock!)
                    e.insert(key);
                    self.reverse_mapping.insert(key, id);

                    self.stats.vectors_added.fetch_add(1, Ordering::Relaxed);
                    return Ok(());
                }
            }
        }
    }

    fn remove(&self, id: NodeId) -> Result<()> {
        // Check for re-entrant modification during filtered search (prevents deadlock)
        if IN_FILTER_CALLBACK.with(|flag| flag.get()) {
            return Err(Error::Vector(VectorError::IndexError(
                "Cannot modify index from within a search_with_filter callback. \
                 This would cause a deadlock due to lock re-entrancy. \
                 Consider collecting modifications and applying them after the search completes."
                    .to_string(),
            )));
        }

        // Check if index is read-only (memory-mapped)
        if self.is_mmap {
            return Err(Error::Vector(VectorError::IndexError(
                "Cannot modify memory-mapped index (read-only)".to_string(),
            )));
        }

        // Find the key for this NodeId
        if let Some((_, key)) = self.id_mapping.remove(&id) {
            self.reverse_mapping.remove(&key);

            // Native delete in usearch
            let index = self.inner.write();
            index.remove(key).map_err(|e| {
                Error::Vector(VectorError::IndexError(format!(
                    "Failed to remove vector: {}",
                    e
                )))
            })?;

            self.stats.vectors_removed.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    }

    fn search(&self, query: &[f32], k: usize) -> Result<Vec<(NodeId, f32)>> {
        // Validate query vector
        validate_vector(query)?;

        // Check dimensions match
        if query.len() != self.config.dimensions {
            return Err(Error::Vector(VectorError::DimensionMismatch {
                expected: self.config.dimensions,
                actual: query.len(),
            }));
        }

        // Cap k to prevent DoS
        let k_capped = k.min(self.max_k);

        // Perform search with retry logic for transient errors
        let index = self.inner.read();

        // Retry with exponential backoff to handle thread pool exhaustion
        // Under heavy concurrent load, usearch may fail with "No available threads to lock"
        for attempt in 0..MAX_SEARCH_ATTEMPTS {
            match index.search(query, k_capped) {
                Ok(matches) => {
                    self.stats
                        .searches_performed
                        .fetch_add(1, Ordering::Relaxed);

                    // Convert and sort results using helper function
                    let results = self.convert_and_sort_matches(matches);
                    return Ok(results);
                }
                Err(e) => {
                    let error_msg = e.to_string();
                    // Check if this is a transient thread pool exhaustion error
                    if is_retryable_usearch_error(&error_msg) && attempt + 1 < MAX_SEARCH_ATTEMPTS {
                        // Track retry for observability
                        self.stats.search_retries.fetch_add(1, Ordering::Relaxed);

                        // Exponential backoff: 1ms, 2ms, 4ms
                        let delay_ms = 1u64 << attempt;
                        std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                        continue;
                    }
                    // Non-retryable error or exhausted retries
                    if attempt > 0 {
                        // Track that we failed even after retries
                        self.stats
                            .search_retry_failures
                            .fetch_add(1, Ordering::Relaxed);
                    }
                    return Err(Error::Vector(VectorError::IndexError(format!(
                        "Search failed: {}",
                        e
                    ))));
                }
            }
        }

        // Unreachable: loop always returns from inside
        unreachable!("Search retry loop should always return from within the loop body")
    }

    fn search_with_filter<F>(
        &self,
        query: &[f32],
        k: usize,
        predicate: F,
    ) -> Result<Vec<(NodeId, f32)>>
    where
        F: Fn(&NodeId) -> bool + Send + Sync,
    {
        // Validate query vector
        validate_vector(query)?;

        if query.len() != self.config.dimensions {
            return Err(Error::Vector(VectorError::DimensionMismatch {
                expected: self.config.dimensions,
                actual: query.len(),
            }));
        }

        let k_capped = k.min(self.max_k);

        // Use usearch's native filtered search with retry logic for thread contention
        let index = self.inner.read();

        // Create a filter that maps usearch keys to our predicate.
        //
        // PERFORMANCE OPTIMIZATION (Issue #206):
        // We retrieve NodeIds from reverse_mapping without validation.
        // This is safe because all NodeIds in reverse_mapping were validated
        // when inserted via add(). The usearch keys come from our own insertions,
        // so we can trust they map to valid NodeIds.
        //
        // This avoids ~1-2ns of validation overhead per candidate node examined.
        // For searches examining 1,000 nodes, this saves ~1-2μs total.
        //
        // DEADLOCK PREVENTION (PR #870):
        // We set IN_FILTER_CALLBACK flag when calling the user's predicate to detect
        // and prevent re-entrant modification attempts that would cause deadlock.
        let reverse_mapping = &self.reverse_mapping;
        let filter = |key: u64| -> bool {
            if let Some(node_id_ref) = reverse_mapping.get(&key) {
                // Set flag to prevent modifications during callback
                let _guard = FilterCallbackGuard::new();
                predicate(node_id_ref.value())
            } else {
                false
            }
        };

        // Retry with exponential backoff to handle thread pool exhaustion
        // Under heavy concurrent load, usearch may fail with "No available threads to lock"
        for attempt in 0..MAX_SEARCH_ATTEMPTS {
            match index.filtered_search(query, k_capped, filter) {
                Ok(matches) => {
                    self.stats
                        .searches_performed
                        .fetch_add(1, Ordering::Relaxed);

                    // Convert and sort results using helper function
                    let results = self.convert_and_sort_matches(matches);
                    return Ok(results);
                }
                Err(e) => {
                    let error_msg = e.to_string();
                    // Check if this is a transient thread pool exhaustion error
                    if is_retryable_usearch_error(&error_msg) && attempt + 1 < MAX_SEARCH_ATTEMPTS {
                        // Track retry for observability
                        self.stats.search_retries.fetch_add(1, Ordering::Relaxed);

                        // Exponential backoff: 1ms, 2ms, 4ms
                        let delay_ms = 1u64 << attempt;
                        std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                        continue;
                    }
                    // Non-retryable error or exhausted retries
                    if attempt > 0 {
                        // Track that we failed even after retries
                        self.stats
                            .search_retry_failures
                            .fetch_add(1, Ordering::Relaxed);
                    }
                    return Err(Error::Vector(VectorError::IndexError(format!(
                        "Filtered search failed: {}",
                        e
                    ))));
                }
            }
        }

        // Unreachable: loop always returns from inside
        unreachable!("Filtered search retry loop should always return from within the loop body")
    }

    fn len(&self) -> usize {
        self.inner.read().size()
    }

    fn dimensions(&self) -> usize {
        self.config.dimensions
    }

    fn distance_metric(&self) -> DistanceMetric {
        self.config.metric
    }

    fn add_batch(&self, items: &[(NodeId, Vec<f32>)]) -> Result<()> {
        for (id, vec) in items {
            self.add(*id, vec)?;
        }
        Ok(())
    }

    fn remove_batch(&self, ids: &[NodeId]) -> Result<()> {
        for id in ids {
            self.remove(*id)?;
        }
        Ok(())
    }

    /// Save the index to disk.
    ///
    /// # Async Runtime Behavior
    ///
    /// This method performs blocking I/O operations (`std::fs::write` and `usearch::Index::save`).
    /// When running within a multi-threaded Tokio runtime (enabled via `tokio` or `embeddings` features),
    /// it automatically uses `tokio::task::block_in_place` to offload this work from the async worker thread.
    ///
    /// This prevents the blocking operation from stalling the async reactor, maintaining responsiveness
    /// for other tasks running on the same thread.
    ///
    /// # Fallback
    ///
    /// If executed outside a Tokio runtime, or in a single-threaded runtime (where `block_in_place`
    /// would panic), it falls back to standard synchronous execution.
    fn save(&self, path: &Path) -> Result<()> {
        #[cfg(any(feature = "tokio", feature = "embeddings"))]
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            // Note: Clippy suggests collapsing this if, but 'let chains' are unstable in this context
            #[allow(clippy::collapsible_if)]
            if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread {
                return tokio::task::block_in_place(|| self.save_internal(path));
            }
        }

        self.save_internal(path)
    }

    fn memory_usage(&self) -> usize {
        self.inner.read().memory_usage()
    }

    fn quantization(&self) -> Quantization {
        self.config.quantization
    }

    fn compact(&self) -> Result<()> {
        // usearch native deletes don't require compaction
        Ok(())
    }
}

// Private helper methods for HnswIndex
impl HnswIndex {
    /// Internal implementation of index saving.
    ///
    /// This method performs the actual blocking I/O operations for saving the index
    /// and its mappings. It is separated from `save()` to allow the latter to use
    /// `tokio::task::block_in_place` when running within a Tokio runtime.
    fn save_internal(&self, path: &Path) -> Result<()> {
        // PHANTOM VECTOR FIX: We must save the index BEFORE collecting mappings.
        //
        // Previous implementation collected mappings first, then saved index.
        // If add() interleaved (added to index but not yet to mappings), we would
        // save the new vector in the index but miss its ID mapping.
        // Result: Phantom Vector (exists in index but invisible/unsearchable).
        //
        // New order:
        // 1. Lock Inner (Read) -> Blocks add()
        // 2. Save Index
        // 3. Drop Inner
        // 4. Collect Mappings
        //
        // If add() happens after step 3, we capture the new ID in mappings.
        // Result: Dangling ID (ID maps to key not in index).
        // This is benign/recoverable (search returns nothing, add overwrites).

        // 1. Acquire inner read lock
        let index = self.inner.read();

        // 2. Save index to disk
        index
            .save(path.to_str().ok_or_else(|| {
                Error::Vector(VectorError::IndexError(
                    "Path contains invalid UTF-8".to_string(),
                ))
            })?)
            .map_err(|e| {
                Error::Vector(VectorError::IndexError(format!(
                    "Failed to save index: {}",
                    e
                )))
            })?;

        // 3. Release lock to allow concurrent operations
        drop(index);

        // 4. Collect mappings
        // Safe to iterate DashMap without holding Inner lock.
        // Even if we held Inner lock, it would be safe now because add() uses Inner -> DM order.
        let mappings: Vec<(NodeId, u64)> = self
            .id_mapping
            .iter()
            .map(|e| (*e.key(), *e.value()))
            .collect();
        let count = mappings.len();

        // Save mappings to companion file with integrity checks
        // Format: [MAGIC:4][VERSION:1][COUNT:8][DATA:16*count][CRC32:4]
        let mappings_path = path.with_extension("usearch.mappings");

        // Calculate total size: Magic(4) + Version(1) + Count(8) + Data(count * 16) + CRC(4)
        // Use checked arithmetic to prevent overflow
        let count_size = count
            .checked_mul(16)
            .ok_or_else(|| Error::Vector(VectorError::IndexError("Index too large".to_string())))?;
        let _total_size = count_size
            .checked_add(4 + 1 + 8 + 4)
            .ok_or_else(|| Error::Vector(VectorError::IndexError("Index too large".to_string())))?;

        // Open file with streaming writer
        let file = File::create(&mappings_path).map_err(|e| {
            Error::Vector(VectorError::IndexError(format!(
                "Failed to create mappings file: {}",
                e
            )))
        })?;
        let mut writer = BufWriter::new(file);

        // Use Vec iterator instead of DashMap iterator
        Self::write_mappings_to_writer(&mut writer, mappings.into_iter(), count)
    }

    /// Helper method to stream mappings to a writer with CRC calculation.
    /// Extracted for testability of error paths.
    fn write_mappings_to_writer<W, I>(writer: &mut W, mappings_iter: I, count: usize) -> Result<()>
    where
        W: Write,
        I: Iterator<Item = (NodeId, u64)>,
    {
        let mut hasher = Hasher::new();
        let count_u64 = count as u64;

        // Helper closure to write and update hasher
        // We cannot use a simple closure that borrows writer mutably because
        // we need to call it multiple times.
        // Instead, we use a macro or just call a helper function.
        // For simplicity and to avoid borrow checker issues with closures capturing mutable refs,
        // we'll implement the logic inline or via a local helper function that takes the writer.

        fn write_and_hash<W: Write>(
            writer: &mut W,
            hasher: &mut Hasher,
            data: &[u8],
        ) -> Result<()> {
            writer.write_all(data).map_err(|e| {
                Error::Vector(VectorError::IndexError(format!(
                    "Failed to write mappings: {}",
                    e
                )))
            })?;
            hasher.update(data);
            Ok(())
        }

        // Write header
        write_and_hash(writer, &mut hasher, MAPPING_MAGIC)?;
        write_and_hash(writer, &mut hasher, &[MAPPING_VERSION])?;
        write_and_hash(writer, &mut hasher, &count_u64.to_le_bytes())?;

        // Write data directly
        for (node_id, key) in mappings_iter {
            write_and_hash(writer, &mut hasher, &node_id.as_u64().to_le_bytes())?;
            write_and_hash(writer, &mut hasher, &key.to_le_bytes())?;
        }

        // Calculate and write CRC32
        let crc = hasher.finalize();
        writer.write_all(&crc.to_le_bytes()).map_err(|e| {
            Error::Vector(VectorError::IndexError(format!(
                "Failed to write CRC: {}",
                e
            )))
        })?;

        writer.flush().map_err(|e| {
            Error::Vector(VectorError::IndexError(format!(
                "Failed to flush mappings: {}",
                e
            )))
        })?;

        Ok(())
    }

    /// Convert usearch matches to sorted vector of (NodeId, similarity) tuples.
    ///
    /// This helper function encapsulates the common logic of:
    /// 1. Converting usearch keys back to NodeIds
    /// 2. Converting distances to similarities based on the configured metric
    /// 3. Sorting results by similarity (descending order)
    ///
    /// # Performance Optimization (Issue #206)
    ///
    /// This method retrieves NodeIds from `reverse_mapping` without validation.
    /// This is safe because:
    /// - All NodeIds in `reverse_mapping` were inserted via the `add()` method
    /// - The `add()` method performs validation when accepting user-provided NodeIds
    /// - Internal key allocation ensures all keys are within valid bounds
    ///
    /// By avoiding `NodeId::new()` validation on every result, we save ~1-2ns per
    /// node examined. For a search examining 1,000 candidates, this saves ~1-2μs.
    ///
    /// # Arguments
    ///
    /// * `matches` - The raw matches from usearch containing keys and distances
    ///
    /// # Returns
    ///
    /// A sorted vector of (NodeId, similarity) pairs where higher similarity means more similar.
    fn convert_and_sort_matches(&self, matches: Matches) -> Vec<(NodeId, f32)> {
        let mut results: Vec<(NodeId, f32)> = Vec::with_capacity(matches.keys.len());

        for (key, distance) in matches.keys.iter().zip(matches.distances.iter()) {
            if let Some(node_id_ref) = self.reverse_mapping.get(key) {
                let node_id = *node_id_ref.value();

                // Convert distance to similarity based on metric.
                // usearch returns distances where lower = more similar (except for some metrics).
                // We convert to similarity where higher = more similar for consistent API.
                //
                // - Cosine: usearch returns cosine distance (1 - cosine_similarity), range [0, 2]
                //   Converting: similarity = 1 - distance, gives cosine similarity in [-1, 1]
                // - Euclidean: usearch returns squared L2 distance, range [0, inf)
                //   Converting: similarity = -distance, so closer vectors have higher similarity
                // - DotProduct: usearch returns -dot_product (negated for min-heap), range (-inf, inf)
                //   Converting: similarity = -distance = dot_product, higher is more similar
                // - Haversine: usearch returns great-circle distance, range [0, pi]
                //   Converting: similarity = -distance, closer points have higher similarity
                // - Hamming: usearch returns bit differences count, range [0, dims]
                //   Converting: similarity = -distance, fewer differences = more similar
                // - Tanimoto: usearch returns Tanimoto distance (1 - coefficient), range [0, 1]
                //   Converting: similarity = 1 - distance = Tanimoto coefficient in [0, 1]
                let similarity = match self.config.metric {
                    DistanceMetric::Cosine => 1.0 - distance,
                    DistanceMetric::Euclidean => -distance,
                    DistanceMetric::DotProduct => -distance,
                    DistanceMetric::Haversine => -distance,
                    DistanceMetric::Hamming => -distance,
                    DistanceMetric::Tanimoto => 1.0 - distance,
                };

                results.push((node_id, similarity));
            }
        }

        // Results should already be sorted by usearch, but ensure descending order by similarity
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        results
    }
}

/// Load and verify mappings from a companion file.
/// Returns (id_mapping, reverse_mapping, max_key) or error if integrity check fails.
/// Format: `[MAGIC:4][VERSION:1][COUNT:8][DATA:16*count][CRC32:4]`
#[allow(clippy::type_complexity)]
fn load_mappings_with_integrity(
    mappings_path: &Path,
) -> Result<(DashMap<NodeId, u64>, DashMap<u64, NodeId>, u64)> {
    let id_mapping = DashMap::new();
    let reverse_mapping = DashMap::new();
    let mut max_key = 0u64;

    if !mappings_path.exists() {
        return Ok((id_mapping, reverse_mapping, max_key));
    }

    // Use streaming (File + BufReader) instead of reading entire file to memory (fs::read).
    // This prevents OOM DoS attacks with large or manipulated files.
    let file = File::open(mappings_path).map_err(|e| {
        Error::Vector(VectorError::IndexError(format!(
            "Failed to open mappings file: {}",
            e
        )))
    })?;

    let file_len = file
        .metadata()
        .map_err(|e| {
            Error::Vector(VectorError::IndexError(format!(
                "Failed to get mappings file metadata: {}",
                e
            )))
        })?
        .len();

    // Minimum size: magic(4) + version(1) + count(8) + crc(4) = 17 bytes
    if file_len < 17 {
        return Err(Error::Vector(VectorError::IndexError(
            "Mapping file too small or corrupted".to_string(),
        )));
    }

    let mut reader = std::io::BufReader::new(file);
    let mut hasher = Hasher::new();

    // 1. Read Header (13 bytes)
    let mut header = [0u8; 13];
    reader.read_exact(&mut header).map_err(|e| {
        Error::Vector(VectorError::IndexError(format!(
            "Failed to read mappings header: {}",
            e
        )))
    })?;

    // Update hasher with header
    hasher.update(&header);

    // Verify magic bytes
    if &header[0..4] != MAPPING_MAGIC {
        return Err(Error::Vector(VectorError::IndexError(
            "Invalid mapping file: bad magic bytes".to_string(),
        )));
    }

    // Check version
    let version = header[4];
    if version != MAPPING_VERSION {
        return Err(Error::Vector(VectorError::IndexError(format!(
            "Unsupported mapping file version: {} (expected {})",
            version, MAPPING_VERSION
        ))));
    }

    // Parse count
    let count = u64::from_le_bytes(header[5..13].try_into().unwrap()) as usize;

    // Verify data size with checked arithmetic
    // Cast to u64 for file size comparison
    let data_size = (count as u64).checked_mul(16).ok_or_else(|| {
        Error::Vector(VectorError::IndexError(
            "Mapping count too large (overflow)".to_string(),
        ))
    })?;
    let expected_size = data_size.checked_add(4 + 1 + 8 + 4).ok_or_else(|| {
        Error::Vector(VectorError::IndexError(
            "Mapping file size too large (overflow)".to_string(),
        ))
    })?;

    // Critical Security Check: Verify file size matches expected size BEFORE reading data.
    // This prevents reading until EOF if the file is truncated or huge.
    if file_len != expected_size {
        return Err(Error::Vector(VectorError::IndexError(format!(
            "Mapping file size mismatch: expected {} bytes, got {}",
            expected_size, file_len
        ))));
    }

    // 2. Read Data
    // We read in chunks to avoid allocating a huge buffer, but large enough for efficiency.
    // 16KB buffer holds 1024 entries.
    const CHUNK_SIZE: usize = 1024 * 16;
    let mut buffer = vec![0u8; CHUNK_SIZE];
    let mut remaining_entries = count;

    while remaining_entries > 0 {
        // Calculate entries for this chunk
        let entries_in_chunk = std::cmp::min(remaining_entries, 1024);
        let bytes_to_read = entries_in_chunk * 16;
        let slice = &mut buffer[0..bytes_to_read];

        reader.read_exact(slice).map_err(|e| {
            Error::Vector(VectorError::IndexError(format!(
                "Failed to read mappings data: {}",
                e
            )))
        })?;

        hasher.update(slice);

        for chunk in slice.chunks_exact(16) {
            let node_id_raw = u64::from_le_bytes(chunk[0..8].try_into().unwrap());
            let key = u64::from_le_bytes(chunk[8..16].try_into().unwrap());

            if let Ok(node_id) = NodeId::new(node_id_raw) {
                id_mapping.insert(node_id, key);
                reverse_mapping.insert(key, node_id);
                max_key = max_key.max(key);
            }
        }

        remaining_entries -= entries_in_chunk;
    }

    // 3. Read and Verify CRC
    let mut crc_buf = [0u8; 4];
    reader.read_exact(&mut crc_buf).map_err(|e| {
        Error::Vector(VectorError::IndexError(format!(
            "Failed to read mappings CRC: {}",
            e
        )))
    })?;

    let stored_crc = u32::from_le_bytes(crc_buf);
    let computed_crc = hasher.finalize();

    if stored_crc != computed_crc {
        return Err(Error::Vector(VectorError::IndexError(format!(
            "Mapping file corrupted: CRC mismatch (stored: {}, computed: {})",
            stored_crc, computed_crc
        ))));
    }

    Ok((id_mapping, reverse_mapping, max_key))
}

impl HnswIndex {
    /// Creates a new HNSW index from a configuration.
    pub fn new(config: HnswConfig) -> Result<Self> {
        HnswIndexBuilder::from_config(&config).build()
    }

    /// Sets the ef_search parameter for query-time search quality.
    pub fn set_ef_search(&self, ef_search: usize) {
        let index = self.inner.read();
        index.change_expansion_search(ef_search);
    }

    /// Gets the current ef_search value.
    ///
    /// Note: Returns the runtime value which may differ from config if
    /// `set_ef_search` was called.
    pub fn get_ef_search(&self) -> usize {
        self.inner.read().expansion_search()
    }

    /// Returns the configuration used to create this index.
    pub fn config(&self) -> HnswConfig {
        self.config.clone()
    }

    /// Returns the M parameter (connections per node).
    pub fn m(&self) -> usize {
        self.config.m
    }

    /// Get all ID mappings for persistence.
    ///
    /// Returns a vector of (node_id, usearch_key) tuples.
    pub(crate) fn get_id_mappings(&self) -> Vec<(u64, u64)> {
        self.id_mapping
            .iter()
            .map(|entry| (entry.key().as_u64(), *entry.value()))
            .collect()
    }

    /// Restore a single ID mapping (used during index loading).
    ///
    /// This directly inserts the mapping without adding vectors to the index.
    pub(crate) fn restore_mapping(&self, node_id: crate::core::id::NodeId, usearch_key: u64) {
        self.id_mapping.insert(node_id, usearch_key);
        self.reverse_mapping.insert(usearch_key, node_id);

        // Update next_key atomically to prevent race conditions during concurrent loading
        // fetch_max ensures we always get the highest key seen, even with concurrent calls
        self.next_key.fetch_max(usearch_key + 1, Ordering::SeqCst);
    }

    /// Loads an index from a file path.
    pub fn load(path: &Path, config: HnswConfig) -> Result<Self> {
        // Security Check: Custom metrics require F32 quantization
        if config.custom_metric.is_some() && config.quantization != Quantization::F32 {
            return Err(Error::Vector(VectorError::InvalidVector {
                reason: format!(
                    "Custom metrics are only supported with F32 quantization (requested {:?}). \
                     Using other quantization levels with custom metrics causes memory safety issues.",
                    config.quantization
                ),
            }));
        }

        let options = IndexOptions {
            dimensions: config.dimensions,
            metric: to_usearch_metric(config.metric),
            quantization: to_usearch_scalar(config.quantization),
            connectivity: config.m,
            expansion_add: config.ef_construction,
            expansion_search: config.ef_search,
            multi: false,
        };

        let mut index = Index::new(&options).map_err(|e| {
            Error::Vector(VectorError::IndexError(format!(
                "Failed to create index for loading: {}",
                e
            )))
        })?;

        index
            .load(path.to_str().ok_or_else(|| {
                Error::Vector(VectorError::IndexError(
                    "Path contains invalid UTF-8".to_string(),
                ))
            })?)
            .map_err(|e| {
                Error::Vector(VectorError::IndexError(format!(
                    "Failed to load index: {}",
                    e
                )))
            })?;

        // Apply custom metric if configured (must happen after load, before use)
        // This ensures custom metrics are preserved across save/load cycles
        if let Some(ref custom) = config.custom_metric {
            let dims = config.dimensions;
            let distance_fn = Arc::clone(&custom.distance_fn);

            // Create a wrapper that converts usearch's raw pointer API to our safe slice API
            let metric_wrapper = create_metric_wrapper(dims, distance_fn);

            index.change_metric(metric_wrapper);
        }

        // Load mappings from companion file with integrity verification
        let mappings_path = path.with_extension("usearch.mappings");
        let (id_mapping, reverse_mapping, max_key) = load_mappings_with_integrity(&mappings_path)?;

        Ok(HnswIndex {
            inner: Arc::new(RwLock::new(index)),
            config,
            id_mapping: Arc::new(id_mapping),
            reverse_mapping: Arc::new(reverse_mapping),
            next_key: AtomicU64::new(max_key + 1),
            stats: Arc::new(IndexStats::default()),
            max_k: MAX_K,
            is_mmap: false,
        })
    }

    /// Opens a memory-mapped index from a file path.
    pub fn open_mmap(path: &Path) -> Result<Self> {
        let index = Index::new(&IndexOptions::default()).map_err(|e| {
            Error::Vector(VectorError::IndexError(format!(
                "Failed to create index: {}",
                e
            )))
        })?;

        index
            .view(path.to_str().ok_or_else(|| {
                Error::Vector(VectorError::IndexError(
                    "Path contains invalid UTF-8".to_string(),
                ))
            })?)
            .map_err(|e| {
                Error::Vector(VectorError::IndexError(format!(
                    "Failed to memory-map index: {}",
                    e
                )))
            })?;

        let dimensions = index.dimensions();
        let connectivity = index.connectivity();

        // Load mappings from companion file with integrity verification
        let mappings_path = path.with_extension("usearch.mappings");
        let (id_mapping, reverse_mapping, max_key) = load_mappings_with_integrity(&mappings_path)?;

        Ok(HnswIndex {
            inner: Arc::new(RwLock::new(index)),
            config: HnswConfig {
                dimensions,
                m: connectivity,
                storage: StorageMode::MemoryMapped {
                    path: path.to_path_buf(),
                },
                ..Default::default()
            },
            id_mapping: Arc::new(id_mapping),
            reverse_mapping: Arc::new(reverse_mapping),
            next_key: AtomicU64::new(max_key + 1),
            stats: Arc::new(IndexStats::default()),
            max_k: MAX_K,
            is_mmap: true,
        })
    }
}

// SAFETY: HnswIndex is safe to send between and share across threads.
//
// Why these manual impls are needed:
// usearch::Index wraps a C++ `index_dense_gt` object via raw pointers, which causes
// the compiler to conservatively refuse auto-deriving Send/Sync. However, the
// underlying C++ implementation is thread-safe:
//
// - usearch internally protects graph modifications with a per-node lock
//   (see usearch C++ source: `index_dense_gt::add`, `search`).
// - The usearch documentation explicitly states that concurrent readers and
//   writers are supported: https://unum-cloud.github.io/usearch/cpp/index.html
//
// Our wrapper adds a second synchronization layer on top for Rust safety:
// 1. `inner: Arc<RwLock<Index>>` - Our RwLock ensures Rust's aliasing rules are
//    upheld. Writes get exclusive access, reads get shared access. This is
//    intentionally redundant with usearch's internal locks so that the Rust
//    borrow checker can verify correctness at our API boundary.
// 2. `id_mapping: Arc<DashMap<NodeId, u64>>` - DashMap is explicitly Send+Sync.
// 3. `reverse_mapping: Arc<DashMap<u64, NodeId>>` - DashMap is explicitly Send+Sync.
// 4. `next_key: AtomicU64` - All atomics are Send+Sync.
// 5. `stats: Arc<IndexStats>` - Contains only AtomicU64 fields.
// 6. `config: HnswConfig` - Contains only Copy/Clone primitive types.
// 7. `max_k: usize`, `is_mmap: bool` - Immutable after construction.
//
// In practice, all HnswIndex instances in AletheiaDB are accessed through
// `Arc<RwLock<HnswIndex>>` (see VectorIndexManager), providing an additional
// synchronization barrier before any field is touched.
//
// References:
// - usearch thread safety docs: https://unum-cloud.github.io/usearch/cpp/index.html
// - usearch C++ uses per-node locks for graph modifications
// - This fork: https://github.com/madmax983/USearch (pinned revision in Cargo.toml)
unsafe impl Send for HnswIndex {}
unsafe impl Sync for HnswIndex {}

#[cfg(test)]
mod sentry_tests {
    use super::*;

    #[test]
    #[should_panic(expected = "usearch passed unaligned pointer")]
    fn test_metric_wrapper_panic_on_unaligned() {
        let distance_fn = Arc::new(|_: &[f32], _: &[f32]| 0.0);
        let wrapper = create_metric_wrapper(4, distance_fn);

        // Create a buffer and get an unaligned pointer
        let buffer = [0u8; 32];
        // Address + 1 is definitely unaligned for f32 (align 4)
        let unaligned_ptr = unsafe { buffer.as_ptr().add(1) } as *const f32;
        let aligned_vec = [0.0f32; 4];
        let aligned_ptr = aligned_vec.as_ptr();

        wrapper(unaligned_ptr, aligned_ptr);
    }

    #[test]
    fn test_is_retryable_error_matching() {
        assert!(is_retryable_usearch_error(
            "Error: No available threads to lock for search"
        ));
        assert!(!is_retryable_usearch_error("Other error"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hnsw_basic() -> Result<()> {
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;

        let node1 = NodeId::new(1).unwrap();
        let node2 = NodeId::new(2).unwrap();

        index.add(node1, &[1.0, 0.0, 0.0, 0.0])?;
        index.add(node2, &[0.0, 1.0, 0.0, 0.0])?;

        assert_eq!(index.len(), 2);

        let results = index.search(&[1.0, 0.0, 0.0, 0.0], 2)?;
        assert_eq!(results[0].0, node1);

        Ok(())
    }

    #[test]
    #[should_panic(expected = "usearch passed unaligned pointer")]
    fn test_metric_wrapper_panic_on_unaligned() {
        // This test ensures that the metric wrapper correctly detects unaligned pointers.
        let distance_fn = Arc::new(|_: &[f32], _: &[f32]| 0.0);
        let wrapper = create_metric_wrapper(4, distance_fn);

        // Create a buffer that we can misalign
        // We need at least 4 f32s (16 bytes) + 1 byte offset
        let mut buffer = vec![0u8; 16 + 8];

        // Get an aligned pointer
        let aligned_ptr = buffer.as_mut_ptr();

        // Create an unaligned pointer by adding 1 byte offset
        // SAFETY: We allocated enough space. This pointer is valid but unaligned for f32.
        let unaligned_ptr = unsafe { aligned_ptr.add(1) } as *const f32;
        let valid_ptr = aligned_ptr as *const f32;

        // Pass unaligned pointer - should panic
        wrapper(valid_ptr, unaligned_ptr);
    }

    #[test]
    fn test_hnsw_remove() -> Result<()> {
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;

        let node1 = NodeId::new(1).unwrap();
        let node2 = NodeId::new(2).unwrap();

        index.add(node1, &[1.0, 0.0, 0.0, 0.0])?;
        index.add(node2, &[0.0, 1.0, 0.0, 0.0])?;

        assert_eq!(index.len(), 2);

        index.remove(node1)?;

        assert_eq!(index.len(), 1);

        let results = index.search(&[1.0, 0.0, 0.0, 0.0], 2)?;
        // Should only return node2 (node1 is deleted)
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, node2);

        Ok(())
    }

    #[test]
    fn test_hnsw_search_with_filter() -> Result<()> {
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;

        let node1 = NodeId::new(1).unwrap();
        let node2 = NodeId::new(2).unwrap();
        let node3 = NodeId::new(3).unwrap();

        index.add(node1, &[1.0, 0.0, 0.0, 0.0])?;
        index.add(node2, &[0.9, 0.1, 0.0, 0.0])?;
        index.add(node3, &[0.8, 0.2, 0.0, 0.0])?;

        // Filter to only even node IDs
        let results =
            index.search_with_filter(&[1.0, 0.0, 0.0, 0.0], 3, |id| id.as_u64() % 2 == 0)?;

        // Should only return node2
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, node2);

        Ok(())
    }

    #[test]
    fn test_hnsw_config_new_fields() {
        let config = HnswConfig::new(384, DistanceMetric::Cosine)
            .with_quantization(Quantization::F16)
            .with_storage(StorageMode::InMemory);

        assert_eq!(config.quantization, Quantization::F16);
        assert!(matches!(config.storage, StorageMode::InMemory));
    }

    #[test]
    fn test_hnsw_config_custom_metric() {
        let config = HnswConfig::new(4, DistanceMetric::Cosine)
            .with_custom_metric("weighted", |a, b| {
                a.iter().zip(b.iter()).map(|(x, y)| (x - y).abs()).sum()
            });

        assert!(config.custom_metric.is_some());
        assert_eq!(config.custom_metric.as_ref().unwrap().name, "weighted");
    }

    #[test]
    fn test_validate_ef_parameters() {
        // Test ef_construction limits
        let result = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .ef_construction(5) // Too small
            .build();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("ef_construction"));

        let result = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .ef_construction(5000) // Too large
            .build();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("ef_construction"));

        // Test ef_search limits
        let result = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .ef_search(0) // Too small
            .build();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("ef_search"));

        let result = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .ef_search(5000) // Too large
            .build();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("ef_search"));
    }

    #[test]
    fn test_distance_to_similarity_conversion() -> Result<()> {
        // Test Cosine similarity conversion
        let cosine_index = HnswIndexBuilder::new(3, DistanceMetric::Cosine).build()?;

        let n1 = NodeId::new(1).unwrap();
        let n2 = NodeId::new(2).unwrap();
        let n3 = NodeId::new(3).unwrap();

        cosine_index.add(n1, &[1.0, 0.0, 0.0])?; // Identical to query
        cosine_index.add(n2, &[0.9, 0.1, 0.0])?; // Very similar
        cosine_index.add(n3, &[0.0, 1.0, 0.0])?; // Orthogonal

        let results = cosine_index.search(&[1.0, 0.0, 0.0], 3)?;

        // Verify similarity values (not distances)
        assert_eq!(results[0].0, n1);
        assert!(results[0].1 > 0.99); // Identical: similarity ~= 1.0

        assert_eq!(results[1].0, n2);
        assert!(results[1].1 > 0.9); // Very similar: similarity > 0.9

        assert_eq!(results[2].0, n3);
        assert!(results[2].1 < 0.1 && results[2].1 > -0.1); // Orthogonal: similarity ~= 0.0

        Ok(())
    }

    #[test]
    fn test_update_existing_node() -> Result<()> {
        // Test the Occupied entry path in add() - updating an existing node
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;

        let node1 = NodeId::new(1).unwrap();

        // Add initial vector
        index.add(node1, &[1.0, 0.0, 0.0, 0.0])?;
        assert_eq!(index.len(), 1);

        // Update with new vector (this exercises the Occupied entry path)
        index.add(node1, &[0.0, 1.0, 0.0, 0.0])?;
        assert_eq!(index.len(), 1); // Still only one node

        // Verify the vector was updated, not duplicated
        let results = index.search(&[0.0, 1.0, 0.0, 0.0], 1)?;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, node1);
        assert!(results[0].1 > 0.99); // Should match the new vector

        Ok(())
    }

    #[test]
    fn test_capacity_expansion_on_add() -> Result<()> {
        // Test capacity expansion during initial adds (Vacant entry path)
        // Start with small initial capacity to force expansion
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .initial_capacity(2) // Start with small capacity
            .build()?;

        // Add first two nodes - should fit in initial capacity
        let node1 = NodeId::new(1).unwrap();
        let node2 = NodeId::new(2).unwrap();
        index.add(node1, &[1.0, 0.0, 0.0, 0.0])?;
        index.add(node2, &[0.0, 1.0, 0.0, 0.0])?;
        assert_eq!(index.len(), 2);

        // Add third node - should trigger capacity expansion code path
        let node3 = NodeId::new(3).unwrap();
        index.add(node3, &[0.0, 0.0, 1.0, 0.0])?;
        assert_eq!(index.len(), 3);

        // Add more nodes to verify expansion worked
        let node4 = NodeId::new(4).unwrap();
        let node5 = NodeId::new(5).unwrap();
        index.add(node4, &[0.0, 0.0, 0.0, 1.0])?;
        index.add(node5, &[0.5, 0.5, 0.0, 0.0])?;
        assert_eq!(index.len(), 5);

        // Verify all nodes are searchable
        let results = index.search(&[1.0, 0.0, 0.0, 0.0], 5)?;
        assert_eq!(results.len(), 5);

        Ok(())
    }

    #[test]
    fn test_capacity_expansion_on_update() -> Result<()> {
        // Test capacity expansion during updates (Occupied entry path with expansion)
        // Start with small capacity to test expansion in update path
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .initial_capacity(2)
            .build()?;

        let node1 = NodeId::new(1).unwrap();
        let node2 = NodeId::new(2).unwrap();

        // Fill to initial capacity
        index.add(node1, &[1.0, 0.0, 0.0, 0.0])?;
        index.add(node2, &[0.0, 1.0, 0.0, 0.0])?;
        assert_eq!(index.len(), 2);

        // Update node1 multiple times (exercises Occupied path)
        index.add(node1, &[0.5, 0.5, 0.0, 0.0])?;
        assert_eq!(index.len(), 2); // Still 2 nodes

        // Add new nodes to trigger and test expansion
        let node3 = NodeId::new(3).unwrap();
        index.add(node3, &[0.0, 0.0, 1.0, 0.0])?;
        assert_eq!(index.len(), 3);

        let node4 = NodeId::new(4).unwrap();
        index.add(node4, &[0.0, 0.0, 0.0, 1.0])?;
        assert_eq!(index.len(), 4);

        // Update again after expansion to test Occupied path with larger capacity
        index.add(node2, &[0.2, 0.8, 0.0, 0.0])?;
        assert_eq!(index.len(), 4); // Still 4 nodes

        // Verify updates worked correctly
        let results = index.search(&[0.5, 0.5, 0.0, 0.0], 1)?;
        assert_eq!(results[0].0, node1);

        let results2 = index.search(&[0.2, 0.8, 0.0, 0.0], 1)?;
        assert_eq!(results2[0].0, node2);

        Ok(())
    }

    #[test]
    fn test_concurrent_update_same_node() -> Result<()> {
        use std::sync::Arc;
        use std::thread;

        // Test the race condition fix - multiple threads updating the same node
        let index = Arc::new(HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?);
        let node1 = NodeId::new(1).unwrap();

        // Add initial vector
        index.add(node1, &[1.0, 0.0, 0.0, 0.0])?;

        let num_threads = 10;
        let updates_per_thread = 10;

        let mut handles = vec![];

        for thread_id in 0..num_threads {
            let index_clone = Arc::clone(&index);
            let handle = thread::spawn(move || {
                for i in 0..updates_per_thread {
                    // Each thread updates the same node with different vectors
                    let val = (thread_id * updates_per_thread + i) as f32 / 100.0;
                    let vector = vec![val, 1.0 - val, 0.0, 0.0];
                    index_clone.add(node1, &vector).unwrap();
                }
            });
            handles.push(handle);
        }

        // Wait for all threads
        for handle in handles {
            handle.join().unwrap();
        }

        // Should still be exactly one node, not duplicates
        assert_eq!(index.len(), 1);

        // Verify the node is still searchable
        let results = index.search(&[0.5, 0.5, 0.0, 0.0], 1)?;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, node1);

        Ok(())
    }

    #[test]
    fn test_concurrent_mixed_operations() -> Result<()> {
        use std::sync::Arc;
        use std::thread;

        // Test concurrent adds and updates to different nodes
        let index = Arc::new(HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?);

        let num_threads = 8;
        let mut handles = vec![];

        for thread_id in 0..num_threads {
            let index_clone = Arc::clone(&index);
            let handle = thread::spawn(move || {
                // Each thread works with its own node
                let node = NodeId::new(thread_id as u64 + 1).unwrap();

                // Add the node
                let vector = vec![thread_id as f32 / num_threads as f32, 0.0, 0.0, 0.0];
                index_clone.add(node, &vector).unwrap();

                // Update it multiple times
                for i in 0..5 {
                    let val = (thread_id as f32 + i as f32) / (num_threads as f32 * 5.0);
                    let updated_vector = vec![val, 1.0 - val, 0.0, 0.0];
                    index_clone.add(node, &updated_vector).unwrap();
                }
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.join().unwrap();
        }

        // Should have exactly num_threads nodes
        assert_eq!(index.len(), num_threads);

        // All nodes should be searchable
        let results = index.search(&[0.5, 0.5, 0.0, 0.0], num_threads)?;
        assert_eq!(results.len(), num_threads);

        Ok(())
    }

    #[test]
    fn test_max_key_overflow_protection() -> Result<()> {
        // Test that we reject IDs that would exceed MAX_VALID_KEY
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;

        // Manually set next_key to exactly at the limit
        // The check is: if current > MAX_VALID_KEY, so:
        // - Setting to MAX_VALID_KEY: next add uses this key (passes), increments to MAX_VALID_KEY+1
        // - Then the following add has current=MAX_VALID_KEY+1 (> MAX_VALID_KEY), so it fails
        const MAX_VALID_KEY: u64 = u64::MAX - 1000;
        index
            .next_key
            .store(MAX_VALID_KEY, std::sync::atomic::Ordering::SeqCst);

        // This should succeed (uses MAX_VALID_KEY, then increments to MAX_VALID_KEY+1)
        let node1 = NodeId::new(1).unwrap();
        assert!(index.add(node1, &[1.0, 0.0, 0.0, 0.0]).is_ok());

        // Now next_key = MAX_VALID_KEY+1, which is > MAX_VALID_KEY, so this should fail
        let node2 = NodeId::new(2).unwrap();
        let result = index.add(node2, &[0.0, 1.0, 0.0, 0.0]);
        assert!(result.is_err());

        if let Err(Error::Vector(VectorError::IndexError(msg))) = result {
            assert!(msg.contains("overflow") || msg.contains("exceeded"));
        } else {
            panic!(
                "Expected IndexError with overflow/exceeded message, got: {:?}",
                result
            );
        }

        // Updating existing node should still work (doesn't allocate new key)
        assert!(index.add(node1, &[0.5, 0.5, 0.0, 0.0]).is_ok());

        Ok(())
    }

    #[test]
    fn test_update_nonexistent_then_exists() -> Result<()> {
        // Test edge case: try to "update" a node that doesn't exist, then add it properly
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;

        let node1 = NodeId::new(1).unwrap();

        // First add creates the node
        index.add(node1, &[1.0, 0.0, 0.0, 0.0])?;
        assert_eq!(index.len(), 1);

        // Second add updates it
        index.add(node1, &[0.0, 1.0, 0.0, 0.0])?;
        assert_eq!(index.len(), 1);

        // Verify it has the updated vector
        let results = index.search(&[0.0, 1.0, 0.0, 0.0], 1)?;
        assert_eq!(results[0].0, node1);
        assert!(results[0].1 > 0.99);

        Ok(())
    }

    #[test]
    fn test_stats_tracking() -> Result<()> {
        // Test that statistics are correctly tracked for adds and updates
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;

        let node1 = NodeId::new(1).unwrap();
        let node2 = NodeId::new(2).unwrap();

        let initial_adds = index
            .stats
            .vectors_added
            .load(std::sync::atomic::Ordering::Relaxed);

        // Add two nodes
        index.add(node1, &[1.0, 0.0, 0.0, 0.0])?;
        index.add(node2, &[0.0, 1.0, 0.0, 0.0])?;

        let after_adds = index
            .stats
            .vectors_added
            .load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(after_adds - initial_adds, 2);

        // Update node1 - should still increment vectors_added counter
        index.add(node1, &[0.5, 0.5, 0.0, 0.0])?;

        let after_update = index
            .stats
            .vectors_added
            .load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(after_update - initial_adds, 3);

        Ok(())
    }

    #[test]
    #[should_panic(expected = "usearch passed null pointer")]
    fn test_metric_wrapper_panic_on_null() {
        // This test ensures that the metric wrapper correctly detects null pointers
        // and panics to prevent UB. This covers the safety check added for FFI.
        let distance_fn = Arc::new(|_: &[f32], _: &[f32]| 0.0);
        let wrapper = create_metric_wrapper(4, distance_fn);

        // Create a valid pointer for one argument
        let vec = [0.0f32; 4];
        let valid_ptr = vec.as_ptr();
        let null_ptr = std::ptr::null();

        // Pass null pointer - should panic
        wrapper(valid_ptr, null_ptr);
    }

    #[test]
    fn test_load_mappings_bad_magic() -> Result<()> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_index.usearch");
        let mappings_path = path.with_extension("usearch.mappings");

        // Create valid index
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;
        index.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0])?;
        index.save(&path)?;

        // Corrupt magic bytes
        let mut data = std::fs::read(&mappings_path).unwrap();
        data[0] = b'X';
        data[1] = b'X';
        data[2] = b'X';
        data[3] = b'X';
        std::fs::write(&mappings_path, &data).unwrap();

        // Try to load
        let result = HnswIndex::load(&path, HnswConfig::new(4, DistanceMetric::Cosine));
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::IndexError(msg))) => {
                assert!(msg.contains("bad magic bytes"));
            }
            Ok(_) => panic!("Expected IndexError with bad magic bytes message, got: Ok(_)"),
            Err(e) => panic!(
                "Expected IndexError with bad magic bytes message, got: Err({:?})",
                e
            ),
        }
        Ok(())
    }

    #[test]
    fn test_load_mappings_bad_version() -> Result<()> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_index.usearch");
        let mappings_path = path.with_extension("usearch.mappings");

        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;
        index.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0])?;
        index.save(&path)?;

        // Corrupt version
        let mut data = std::fs::read(&mappings_path).unwrap();
        data[4] = 99; // Invalid version
        std::fs::write(&mappings_path, &data).unwrap();

        let result = HnswIndex::load(&path, HnswConfig::new(4, DistanceMetric::Cosine));
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::IndexError(msg))) => {
                assert!(msg.contains("Unsupported mapping file version"));
            }
            Ok(_) => panic!("Expected IndexError with version message, got: Ok(_)"),
            Err(e) => panic!(
                "Expected IndexError with version message, got: Err({:?})",
                e
            ),
        }
        Ok(())
    }

    #[test]
    fn test_load_mappings_bad_crc() -> Result<()> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_index.usearch");
        let mappings_path = path.with_extension("usearch.mappings");

        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;
        index.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0])?;
        index.save(&path)?;

        // Corrupt data (which invalidates CRC)
        let mut data = std::fs::read(&mappings_path).unwrap();
        // Modify the node ID part of the data (after header: 4+1+8 = 13 bytes)
        // Data format: [NodeId:8][Key:8]...
        if data.len() > 13 {
            data[13] = data[13].wrapping_add(1);
        }
        std::fs::write(&mappings_path, &data).unwrap();

        let result = HnswIndex::load(&path, HnswConfig::new(4, DistanceMetric::Cosine));
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::IndexError(msg))) => {
                assert!(msg.contains("CRC mismatch"));
            }
            Ok(_) => panic!("Expected IndexError with CRC message, got: Ok(_)"),
            Err(e) => panic!("Expected IndexError with CRC message, got: Err({:?})", e),
        }
        Ok(())
    }

    #[test]
    fn test_load_mappings_truncated() -> Result<()> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_index.usearch");
        let mappings_path = path.with_extension("usearch.mappings");

        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;
        index.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0])?;
        index.save(&path)?;

        // Truncate file
        let data = std::fs::read(&mappings_path).unwrap();
        let truncated = &data[..10]; // Smaller than header
        std::fs::write(&mappings_path, truncated).unwrap();

        let result = HnswIndex::load(&path, HnswConfig::new(4, DistanceMetric::Cosine));
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::IndexError(msg))) => {
                assert!(msg.contains("too small") || msg.contains("corrupted"));
            }
            Ok(_) => panic!("Expected IndexError with size message, got: Ok(_)"),
            Err(e) => panic!("Expected IndexError with size message, got: Err({:?})", e),
        }
        Ok(())
    }

    #[test]
    fn test_load_mappings_size_mismatch() -> Result<()> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_index.usearch");
        let mappings_path = path.with_extension("usearch.mappings");

        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;
        index.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0])?;
        index.save(&path)?;

        // Modify count to be larger (mismatch with actual size), then fix CRC to pass CRC check
        let mut data = std::fs::read(&mappings_path).unwrap();
        // Count is at offset 5 (Magic 4 + Version 1)
        // Original count is 1. Let's make it 2.
        let count_offset = 5;
        data[count_offset] = 2;

        // Recompute CRC so we pass the CRC check and hit the size check
        let crc_offset = data.len() - 4;
        let mut hasher = Hasher::new();
        hasher.update(&data[..crc_offset]);
        let new_crc = hasher.finalize();

        let crc_bytes = new_crc.to_le_bytes();
        data[crc_offset] = crc_bytes[0];
        data[crc_offset + 1] = crc_bytes[1];
        data[crc_offset + 2] = crc_bytes[2];
        data[crc_offset + 3] = crc_bytes[3];

        std::fs::write(&mappings_path, &data).unwrap();

        let result = HnswIndex::load(&path, HnswConfig::new(4, DistanceMetric::Cosine));
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::IndexError(msg))) => {
                assert!(msg.contains("size mismatch"));
            }
            Ok(_) => panic!("Expected IndexError with size mismatch message, got: Ok(_)"),
            Err(e) => panic!(
                "Expected IndexError with size mismatch message, got: Err({:?})",
                e
            ),
        }
        Ok(())
    }

    #[test]
    fn test_load_mappings_overflow_header() -> Result<()> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_index.usearch");
        let mappings_path = path.with_extension("usearch.mappings");

        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;
        index.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0])?;
        index.save(&path)?;

        // Modify count to be HUGE (u64::MAX) to trigger arithmetic overflow check
        let mut data = std::fs::read(&mappings_path).unwrap();
        let count_offset = 5;
        let huge_count = u64::MAX;

        // Write huge count
        let count_bytes = huge_count.to_le_bytes();
        data[count_offset..count_offset + 8].copy_from_slice(&count_bytes);

        // Update CRC (checksum calculation is still valid, only logic check fails)
        let crc_offset = data.len() - 4;
        let mut hasher = Hasher::new();
        hasher.update(&data[..crc_offset]);
        let new_crc = hasher.finalize();
        data[crc_offset..].copy_from_slice(&new_crc.to_le_bytes());

        std::fs::write(&mappings_path, &data).unwrap();

        let result = HnswIndex::load(&path, HnswConfig::new(4, DistanceMetric::Cosine));
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::IndexError(msg))) => {
                assert!(
                    msg.contains("overflow"),
                    "Expected overflow error, got: {}",
                    msg
                );
            }
            Ok(_) => panic!("Expected IndexError with overflow message, got: Ok(_)"),
            Err(e) => panic!(
                "Expected IndexError with overflow message, got: Err({:?})",
                e
            ),
        }
        Ok(())
    }

    #[test]
    fn test_save_mappings_large_streaming() -> Result<()> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_streaming.usearch");

        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;

        // Add enough items to exceed typical buffer sizes (e.g. 8KB)
        // 2000 items * 16 bytes = 32KB
        let count = 2000;
        for i in 1..=count {
            index.add(NodeId::new(i).unwrap(), &[1.0, 0.0, 0.0, 0.0])?;
        }

        index.save(&path)?;

        // Verify we can load it back
        let loaded = HnswIndex::load(&path, HnswConfig::new(4, DistanceMetric::Cosine))?;
        assert_eq!(loaded.len(), count as usize);

        // Verify a few items
        let results = loaded.search(&[1.0, 0.0, 0.0, 0.0], 1)?;
        assert!(!results.is_empty());

        Ok(())
    }

    // Mock writer that fails after writing N bytes
    struct MockFailWriter {
        fail_after: usize,
        written: usize,
    }

    impl MockFailWriter {
        fn new(fail_after: usize) -> Self {
            Self {
                fail_after,
                written: 0,
            }
        }
    }

    impl std::io::Write for MockFailWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if self.written + buf.len() > self.fail_after {
                return Err(std::io::Error::other("Mock write error"));
            }
            self.written += buf.len();
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            // Can simulate flush failure if needed, but write failure is sufficient for coverage
            Ok(())
        }
    }

    #[test]
    fn test_save_mappings_write_errors() {
        // Create dummy mappings
        let mappings = [
            (NodeId::new(1).unwrap(), 100),
            (NodeId::new(2).unwrap(), 200),
        ];

        // Case 1: Fail during header (MAGIC)
        // Magic is 4 bytes. Fail at byte 3.
        let mut writer = MockFailWriter::new(3);
        let result = HnswIndex::write_mappings_to_writer(
            &mut writer,
            mappings.iter().copied(),
            mappings.len(),
        );
        assert!(result.is_err());
        if let Err(Error::Vector(VectorError::IndexError(msg))) = result {
            assert!(msg.contains("Failed to write mappings"));
        } else {
            panic!("Expected IndexError");
        }

        // Case 2: Fail during data writing
        // Magic(4) + Version(1) + Count(8) = 13 bytes header
        // Data is 16 bytes per item.
        // Fail after header + 1st item (16 bytes) + 1 byte
        let mut writer = MockFailWriter::new(13 + 16 + 1);
        let result = HnswIndex::write_mappings_to_writer(
            &mut writer,
            mappings.iter().copied(),
            mappings.len(),
        );
        assert!(result.is_err());
        if let Err(Error::Vector(VectorError::IndexError(msg))) = result {
            assert!(msg.contains("Failed to write mappings"));
        }

        // Case 3: Fail during CRC writing
        // Total data size = 13 + 32 = 45 bytes.
        // CRC is 4 bytes.
        // Fail at 45 + 1 byte (during CRC write)
        let mut writer = MockFailWriter::new(45 + 1);
        let result = HnswIndex::write_mappings_to_writer(
            &mut writer,
            mappings.iter().copied(),
            mappings.len(),
        );
        assert!(result.is_err());
        if let Err(Error::Vector(VectorError::IndexError(msg))) = result {
            assert!(msg.contains("Failed to write CRC"));
        }
    }

    struct MockFlushFailWriter;
    impl std::io::Write for MockFlushFailWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::other("Mock flush error"))
        }
    }

    #[test]
    fn test_save_mappings_flush_error() {
        let mappings = [];
        let mut writer = MockFlushFailWriter;
        let result = HnswIndex::write_mappings_to_writer(
            &mut writer,
            mappings.iter().copied(),
            mappings.len(),
        );
        assert!(result.is_err());
        if let Err(Error::Vector(VectorError::IndexError(msg))) = result {
            assert!(msg.contains("Failed to flush mappings"));
        } else {
            panic!("Expected IndexError");
        }
    }

    #[test]
    fn test_save_mappings_file_create_error() {
        let dir = tempfile::tempdir().unwrap();
        // Create index
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap();

        // Path for the index file
        let index_path = dir.path().join("test.index");

        // Create a directory where the mappings file should be
        // Mappings file path will be "test.usearch.mappings" (since extension replaces "index")
        // Wait, .with_extension("usearch.mappings") replaces "index".
        let mappings_path = index_path.with_extension("usearch.mappings");
        std::fs::create_dir(&mappings_path).unwrap();

        // Attempt to save to index_path.
        // This will try to create mappings file at `mappings_path`, which is a directory.
        // File::create should fail.
        let result = index.save(&index_path);

        assert!(result.is_err());
        if let Err(Error::Vector(VectorError::IndexError(msg))) = result {
            assert!(msg.contains("Failed to create mappings file"));
        } else {
            // Note: Depending on OS, saving the index itself might fail first if index_path is valid but related calls fail.
            // But here index_path is valid (does not exist). usearch index save should succeed.
            // Then save_mappings should fail.
            panic!("Expected IndexError, got {:?}", result);
        }
    }

    #[test]
    fn test_havoc_add_contention_coverage() {
        // This test forces the "Vacant -> Occupied" retry loop in add().
        // Multiple threads race to add the SAME node ID.
        let index = Arc::new(
            HnswIndexBuilder::new(4, DistanceMetric::Cosine)
                .build()
                .unwrap(),
        );
        let node_id = NodeId::new(1).unwrap();
        let vector = vec![1.0, 0.0, 0.0, 0.0];

        let num_threads = 20;
        let mut handles = vec![];

        // Barrier to synchronize start for maximum contention
        let barrier = Arc::new(std::sync::Barrier::new(num_threads));

        for _ in 0..num_threads {
            let index_clone = Arc::clone(&index);
            let vector_clone = vector.clone();
            let barrier_clone = Arc::clone(&barrier);

            handles.push(std::thread::spawn(move || {
                barrier_clone.wait();
                // Everyone tries to add the same node
                index_clone.add(node_id, &vector_clone).unwrap();
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        assert_eq!(index.len(), 1);
    }

    #[test]
    fn test_havoc_save_index_failure_coverage() {
        // Test failure path when saving index
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap();
        let node_id = NodeId::new(1).unwrap();
        index.add(node_id, &[1.0, 0.0, 0.0, 0.0]).unwrap();

        // Try to save to a path in a non-existent directory
        // This should fail at index.save() step
        let path = std::path::Path::new("/non_existent_directory_xyz/test.index");
        let result = index.save(path);

        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::IndexError(msg))) => {
                // Verify it's the expected error message from save_internal
                assert!(msg.contains("Failed to save index"));
            }
            _ => panic!("Expected IndexError, got {:?}", result),
        }
    }

    #[test]
    fn test_havoc_add_remove_race() {
        // This test attempts to trigger the "Occupied" path retry loop where
        // the mapping exists initially but is removed/changed before the inner lock is acquired.
        let index = Arc::new(
            HnswIndexBuilder::new(4, DistanceMetric::Cosine)
                .build()
                .unwrap(),
        );
        let node_id = NodeId::new(1).unwrap();
        let vector = vec![1.0, 0.0, 0.0, 0.0];

        // We need extreme contention to hit the tiny race window between
        // "check id_mapping" and "acquire inner lock".
        let num_pairs = 4; // 4 pairs of Adder/Remover threads
        let mut handles = vec![];
        let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let barrier = Arc::new(std::sync::Barrier::new(num_pairs * 2));

        for _ in 0..num_pairs {
            // Thread A: Adds/Updates repeatedly
            let index_a = Arc::clone(&index);
            let vector_a = vector.clone();
            let running_a = Arc::clone(&running);
            let barrier_a = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier_a.wait();
                while running_a.load(std::sync::atomic::Ordering::Relaxed) {
                    let _ = index_a.add(node_id, &vector_a);
                }
            }));

            // Thread B: Removes repeatedly
            let index_b = Arc::clone(&index);
            let running_b = Arc::clone(&running);
            let barrier_b = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier_b.wait();
                while running_b.load(std::sync::atomic::Ordering::Relaxed) {
                    let _ = index_b.remove(node_id);
                }
            }));
        }

        // Let them fight for a longer duration to increase probability of hitting the race
        std::thread::sleep(std::time::Duration::from_millis(500));
        running.store(false, std::sync::atomic::Ordering::Relaxed);

        for handle in handles {
            handle.join().unwrap();
        }
    }

    #[test]
    #[cfg(unix)]
    fn test_save_invalid_utf8_path() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap();

        // Create path with invalid UTF-8 bytes (0xFF)
        let bytes = b"test_invalid_utf8_\xff.index";
        let os_str = OsStr::from_bytes(bytes);
        let path = std::path::Path::new(os_str);

        let result = index.save(path);
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::IndexError(msg))) => {
                assert!(msg.contains("Path contains invalid UTF-8"));
            }
            _ => panic!("Expected invalid UTF-8 error, got {:?}", result),
        }
    }

    #[test]
    fn test_save_internal_write_mappings_error() {
        // Test failure when writing mappings file specifically.
        // We create a directory where the mappings file should be, forcing open/create to fail.
        let dir = tempfile::tempdir().unwrap();
        let index_path = dir.path().join("test_write_map.index");

        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap();
        let node_id = NodeId::new(1).unwrap();
        index.add(node_id, &[1.0, 0.0, 0.0, 0.0]).unwrap();

        // Save index first to ensure that part succeeds
        // We do this manually to setup the state where index exists but mappings creation will fail

        // Block mappings file creation
        let mappings_path = index_path.with_extension("usearch.mappings");
        std::fs::create_dir(&mappings_path).unwrap();
        // Make it read-only directory so we can't overwrite it (if logic tried to)
        let mut perms = std::fs::metadata(&mappings_path).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&mappings_path, perms).unwrap();

        let result = index.save(&index_path);

        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::IndexError(msg))) => {
                assert!(msg.contains("Failed to create mappings file"));
            }
            _ => panic!("Expected IndexError, got {:?}", result),
        }
    }

    #[test]
    fn test_vacant_path_key_allocation_error() {
        // Test error handling in Vacant path key allocation logic.
        // We need to exhaust the key space to trigger the error.
        // Since we can't easily iterate 2^64 times, we'll manually set the next_key to MAX_VALID_KEY.

        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap();

        // MAX_VALID_KEY is u64::MAX - 1000
        let max_valid = u64::MAX - 1000;
        index
            .next_key
            .store(max_valid + 1, std::sync::atomic::Ordering::SeqCst);

        let node_id = NodeId::new(1).unwrap();
        let vector = vec![1.0, 0.0, 0.0, 0.0];

        // add() should fail with overflow protection error
        let result = index.add(node_id, &vector);

        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::IndexError(msg))) => {
                assert!(msg.contains("Maximum number of vectors exceeded"));
            }
            _ => panic!("Expected overflow error, got {:?}", result),
        }
    }
}
