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
//! # fn example() -> aletheiadb::core::error::Result<()> {
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

use crate::core::error::{Error, Result, VectorError};
use crate::core::id::NodeId;
use crate::core::property::MAX_VECTOR_DIMENSIONS;
use crate::core::vector::validate_vector;
use crate::index::vector::{CustomMetric, DistanceMetric, Quantization, StorageMode, VectorIndex};
use crc32fast::Hasher;
use dashmap::DashMap;
use parking_lot::{Mutex, RwLock};
use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use usearch::{Index, IndexOptions, MetricKind, ScalarKind, ffi::Matches};

// Thread-local flag to detect re-entrant modification attempts during filtered search.
// This prevents deadlocks when user filter callbacks try to modify the index.
std::thread_local! {
    pub(crate) static IN_FILTER_CALLBACK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
type TestRaceHook = fn(&HnswIndex, NodeId);

#[cfg(test)]
std::thread_local! {
    // Hook to simulate race conditions in add() Occupied path.
    // Takes the HnswIndex instance and the NodeId being added.
    static TEST_RACE_HOOK: std::cell::Cell<Option<TestRaceHook>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(crate) static TEST_SKIP_CAPACITY_CHECK: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// RAII guard that sets IN_FILTER_CALLBACK to true on creation and restores previous value on drop.
/// This ensures the flag is always reset, even if the callback panics.
pub(crate) struct FilterCallbackGuard {
    prev: bool,
}

impl FilterCallbackGuard {
    pub(crate) fn new() -> Self {
        let prev = IN_FILTER_CALLBACK.with(|flag| flag.replace(true));
        FilterCallbackGuard { prev }
    }
}

impl Drop for FilterCallbackGuard {
    fn drop(&mut self) {
        IN_FILTER_CALLBACK.with(|flag| flag.set(self.prev));
    }
}

/// Magic bytes for mapping file identification
const MAPPING_MAGIC: &[u8; 4] = b"GMAP";
/// Current mapping file format version
const MAPPING_VERSION: u8 = 2;

/// Metadata stored in the mappings file (Version 2+)
struct IndexMetadata {
    dimensions: usize,
    quantization: Quantization,
    metric: DistanceMetric,
}

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

/// Maximum number of entries allowed in a mappings file.
///
/// This limit prevents Memory Exhaustion DoS attacks where a malicious actor
/// provides a sparse mappings file with a header claiming billions of entries.
/// Loading such a file would cause `load_mappings_with_integrity` to attempt
/// allocating massive amounts of memory for the ID mapping `DashMap`.
///
/// Set to 100 Million (100_000_000), which is well above reasonable single-index limits
/// but low enough to prevent catastrophic OOM on typical servers.
/// 100M entries * (16 bytes data + ~32 bytes DashMap overhead) ≈ 4.8GB RAM.
const MAX_MAPPINGS_COUNT: usize = 100_000_000;

/// Number of sharded locks for entry updates.
/// 64 locks provide reasonable contention reduction for concurrent updates.
const NUM_ENTRY_LOCKS: usize = 64;

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
        writer.write_all(&[self.quantization.to_u8()])?;
        Ok(())
    }

    /// Deserialize configuration from a reader.
    pub fn deserialize_from<R: Read>(reader: &mut R) -> Result<Self> {
        let mut buf_u64 = [0u8; 8];
        let mut buf_u8 = [0u8; 1];

        reader.read_exact(&mut buf_u64)?;
        let dimensions = u64::from_le_bytes(buf_u64) as usize;

        if dimensions > MAX_VECTOR_DIMENSIONS {
            return Err(Error::Vector(VectorError::InvalidVector {
                reason: format!(
                    "dimensions {} exceeds maximum allowed {}",
                    dimensions, MAX_VECTOR_DIMENSIONS
                ),
            }));
        }

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

        // Try read quantization (for backward compatibility)
        let quantization = match reader.read_exact(&mut buf_u8) {
            Ok(_) => Quantization::from_u8(buf_u8[0])?,
            Err(ref e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                // Old format (v1) didn't have quantization, assume F32 default
                Quantization::default()
            }
            Err(e) => return Err(e.into()),
        };

        Ok(HnswConfig {
            dimensions,
            metric,
            m,
            ef_construction,
            ef_search,
            capacity,
            quantization,
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
            // If it does, we return f32::MAX to avoid crashing/UB.
            // We cannot return an error here because the signature is fixed by usearch trait.
            eprintln!("usearch passed null pointer to metric function - returning max distance");
            return f32::MAX;
        }

        // Check for alignment to prevent UB
        // Use bitwise check for power-of-2 alignment (f32 align is 4)
        let align_mask = std::mem::align_of::<f32>() - 1;
        if (a as usize) & align_mask != 0 || (b as usize) & align_mask != 0 {
            eprintln!(
                "usearch passed unaligned pointer to metric function (expected alignment {}) - returning max distance",
                std::mem::align_of::<f32>()
            );
            return f32::MAX;
        }

        // SAFETY: usearch guarantees pointers are valid for `dims` elements.
        // We verified they are not null above.

        let slice_a = unsafe { std::slice::from_raw_parts(a, dims) };
        let slice_b = unsafe { std::slice::from_raw_parts(b, dims) };

        // SAFETY: We wrap the user-provided closure in catch_unwind to prevent
        // panics from unwinding across the FFI boundary into C++ code, which is UB.
        // If a panic occurs, we return f32::MAX (infinite distance) to effectively
        // ignore this comparison.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            distance_fn(slice_a, slice_b)
        }));

        match result {
            Ok(val) => val,
            Err(_) => {
                // Log error to stderr so operator is aware of the issue
                eprintln!(
                    "Panic in custom metric function - returning max distance to avoid FFI UB"
                );
                f32::MAX
            }
        }
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
        if self.config.dimensions > MAX_VECTOR_DIMENSIONS {
            return Err(Error::Vector(VectorError::InvalidVector {
                reason: format!(
                    "dimensions {} exceeds maximum allowed {}",
                    self.config.dimensions, MAX_VECTOR_DIMENSIONS
                ),
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
            save_lock: Arc::new(RwLock::new(())),
            entry_locks: (0..NUM_ENTRY_LOCKS).map(|_| Mutex::new(())).collect(),
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
    /// Lock to ensure consistency between index and mapping for saving.
    /// - add/remove: acquires read lock (shared)
    /// - save: acquires write lock (exclusive)
    ///
    /// This allows concurrent adds and searches (resolving deadlock in PR #870),
    /// while preventing concurrent add/save (which causes Zombie Vectors).
    save_lock: Arc<RwLock<()>>,
    /// Sharded locks to serialize updates to the same key/node.
    /// This prevents race conditions during the `remove` -> `add` sequence for updates,
    /// where multiple threads updating the same node could confuse `usearch`.
    entry_locks: Vec<Mutex<()>>,
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

        // Ensure capacity before acquiring any locks that might cause contention
        self.check_and_expand_capacity(1)?;

        // Acquire entry lock to serialize updates to same key
        // This prevents race conditions in usearch when multiple threads update the same key concurrently
        let lock_idx = (id.as_u64() as usize) % self.entry_locks.len();
        let _key_guard = self.entry_locks[lock_idx].lock();

        // Acquire save_lock (shared) to prevent concurrent saves (Zombie Vectors).
        // CRITICAL: Must be acquired BEFORE interacting with id_mapping to ensure
        // atomic update of (mapping + index) relative to save().
        let _save_guard = self.save_lock.read();

        // Get or create key for this NodeId
        // Use entry API for atomic check-and-update to prevent race conditions
        match self.id_mapping.entry(id) {
            dashmap::mapref::entry::Entry::Occupied(entry) => {
                // Re-adding existing node: remove old vector from usearch if it exists
                // Optimization (Issue #207): Only call remove() if key actually exists in usearch.
                // This avoids unnecessary FFI calls during recovery or when mappings are out of sync.
                let existing_key = *entry.get();

                // DEADLOCK FIX (Havoc): Release DashMap lock before acquiring Inner lock.
                // This prevents deadlock with Vacant path which acquires Inner -> Map.
                // We must re-verify the mapping after acquiring Inner lock.
                drop(entry);

                #[cfg(test)]
                {
                    // Hook to simulate race condition: simulate another thread changing/removing mapping
                    // after we dropped the lock but before we acquired inner lock.
                    if let Some(hook) = TEST_RACE_HOOK.with(|h| h.get()) {
                        hook(self, id);
                    }
                }

                // Acquire inner write lock to serialize structural mutations in usearch.
                let index = self.inner.write();

                // Re-verify mapping under Inner lock
                // We need to lock the map again to check if the ID still maps to existing_key.
                // This is safe because we hold Inner, so we are following Inner->Map order.
                if let Some(current_entry) = self.id_mapping.get(&id) {
                    if *current_entry != existing_key {
                        // Mapping changed (concurrent update).
                        // Since we don't hold the correct key anymore, we return a transient error.
                        return Err(Error::Vector(VectorError::IndexError(
                            "Concurrent modification detected during update (mapping changed)"
                                .to_string(),
                        )));
                    }
                    // Mapping is consistent. Proceed with update.
                } else {
                    // Mapping removed.
                    // If the node was deleted, we shouldn't add a vector for it.
                    return Err(Error::Vector(VectorError::IndexError(
                        "Concurrent modification detected during update (node removed)".to_string(),
                    )));
                }

                // Check if key exists before removing to avoid wasteful FFI call
                if index.contains(existing_key) {
                    // Key exists in usearch - remove it before re-adding
                    // (usearch requires explicit remove before add with same key)
                    self.retry_usearch(
                        || index.remove(existing_key),
                        "Failed to remove existing vector",
                    )?;
                } else {
                    // Key doesn't exist, so this is a net addition.
                    // We must ensure capacity exists, as the optimistic check in
                    // check_and_expand_capacity might have been raced.
                    if index.size() >= index.capacity() {
                        let new_capacity = (index.capacity() * 2).max(1024);
                        self.retry_usearch(
                            || index.reserve(new_capacity),
                            "Failed to expand capacity (race recovery)",
                        )?;
                    }
                }
                // Note: If key doesn't exist, we skip remove() and proceed directly to add()
                // This is safe because add() with a non-existent key will succeed

                // Add the new vector while still holding the lock
                if let Err(e) =
                    self.retry_usearch(|| index.add(existing_key, vector), "Failed to add vector")
                {
                    // Check for "Duplicate keys" error which indicates inconsistency between contains() and add()
                    if e.to_string().contains("Duplicate keys") {
                        // Force remove and retry add
                        self.retry_usearch(
                            || index.remove(existing_key),
                            "Failed to force remove existing vector",
                        )?;
                        self.retry_usearch(
                            || index.add(existing_key, vector),
                            "Failed to add vector after force remove",
                        )?;
                    } else {
                        return Err(e);
                    }
                }

                self.stats.vectors_added.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            dashmap::mapref::entry::Entry::Vacant(entry) => {
                // New node: allocate key with overflow protection
                // Check BEFORE incrementing to avoid leaving next_key in invalid state
                const MAX_VALID_KEY: u64 = u64::MAX - 1000;

                // CRITICAL: Drop the entry to release DashMap lock BEFORE acquiring inner lock
                // This prevents lock ordering inversion (dashmap -> inner is FORBIDDEN)
                drop(entry);

                // Step 1: Atomically allocate a unique key (no locks held)
                let key = loop {
                    let current = self.next_key.load(Ordering::SeqCst);
                    if current > MAX_VALID_KEY {
                        return Err(Error::Vector(VectorError::IndexError(
                            "Maximum number of vectors exceeded (key overflow protection)"
                                .to_string(),
                        )));
                    }
                    // Try to atomically increment; retry if another thread beat us
                    match self.next_key.compare_exchange(
                        current,
                        current + 1,
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                    ) {
                        Ok(key) => break key,
                        Err(_) => continue, // Retry with new current value
                    }
                };

                // Note: save_lock is already held at start of function.

                // Step 2: Acquire inner write lock FIRST (follows lock ordering invariant).
                // Vacant path updates the index before claiming the map entry (Inner -> Map).
                let index = self.inner.write();

                // Ensure capacity exists. The optimistic check in check_and_expand_capacity
                // might have been raced by other threads. Since we hold the write lock now,
                // we are the source of truth.
                if index.size() >= index.capacity() {
                    let new_capacity = (index.capacity() * 2).max(1024);
                    self.retry_usearch(
                        || index.reserve(new_capacity),
                        "Failed to expand capacity (race recovery)",
                    )?;
                }

                // Step 3: Add to inner usearch index while holding write lock
                self.retry_usearch(|| index.add(key, vector), "Failed to add vector")?;

                #[cfg(test)]
                {
                    // Hook to simulate race condition: simulate another thread adding mapping
                    // after we checked it was vacant but before we inserted our mapping.
                    if let Some(hook) = TEST_RACE_HOOK.with(|h| h.get()) {
                        hook(self, id);
                    }
                }

                // Step 4: Insert to mappings (dashmap) WHILE HOLDING INNER LOCK
                // We keep the inner lock held to ensure atomicity with respect to save_internal().
                // If we dropped the lock here, save_internal() could run, see the new vector in inner,
                // but miss the mapping in id_mapping (Zombie Vector bug).
                //
                // Lock order check: Inner -> Map (via entry()). This is consistent with other operations.
                let race_detected = match self.id_mapping.entry(id) {
                    dashmap::mapref::entry::Entry::Occupied(_) => true,
                    dashmap::mapref::entry::Entry::Vacant(e) => {
                        // Success: we claimed the ID
                        e.insert(key);
                        // Drop the entry lock implicitly here when e is consumed/scope ends
                        false
                    }
                };

                if race_detected {
                    // Race detected: Another thread added this NodeId concurrently
                    // Our vector is in inner with key=key, but someone else claimed the ID.
                    // We must rollback our addition to avoid phantom vectors.

                    // We already hold the inner write lock, so we can remove directly.
                    self.retry_usearch(
                        || index.remove(key),
                        "Failed to rollback vector after concurrent add",
                    )?;

                    // The existing mapping wins; return error to indicate retry needed
                    return Err(Error::Vector(VectorError::IndexError(
                        "Concurrent add detected for same NodeId, vector already exists"
                            .to_string(),
                    )));
                }

                // If no race, we successfully inserted into id_mapping.
                // Now insert reverse mapping.
                self.reverse_mapping.insert(key, id);
                self.stats.vectors_added.fetch_add(1, Ordering::Relaxed);

                // Explicitly drop index lock (though it would drop at end of scope)
                drop(index);

                Ok(())
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

        // Acquire entry lock to serialize updates to same key
        let lock_idx = (id.as_u64() as usize) % self.entry_locks.len();
        let _key_guard = self.entry_locks[lock_idx].lock();

        // Acquire save_lock (shared) to prevent concurrent saves (Zombie Vectors).
        // Must be acquired BEFORE modifying id_mapping to ensure atomicity with save().
        let _save_guard = self.save_lock.read();

        // Find the key for this NodeId
        if let Some((_, key)) = self.id_mapping.remove(&id) {
            self.reverse_mapping.remove(&key);

            // Native delete in usearch (serialize with other structural mutations)
            let index = self.inner.write();
            self.retry_usearch(|| index.remove(key), "Failed to remove vector")?;

            self.stats.vectors_removed.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    }

    fn search(&self, query: &[f32], k: usize) -> Result<Vec<(NodeId, f32)>> {
        // Check for re-entrant access during filtered search (prevents deadlock)
        if IN_FILTER_CALLBACK.with(|flag| flag.get()) {
            return Err(Error::Vector(VectorError::IndexError(
                "Cannot perform search from within a search_with_filter callback. \
                 This prevents deadlocks when concurrent writers are pending."
                    .to_string(),
            )));
        }

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

                    // Convert results using helper function (results are already sorted by usearch)
                    let results = self.convert_matches(matches);
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
        // Check for re-entrant access during filtered search (prevents deadlock)
        if IN_FILTER_CALLBACK.with(|flag| flag.get()) {
            return Err(Error::Vector(VectorError::IndexError(
                "Cannot perform search_with_filter from within a search_with_filter callback. \
                 This prevents deadlocks when concurrent writers are pending."
                    .to_string(),
            )));
        }

        // Validate query vector
        validate_vector(query)?;

        if query.len() != self.config.dimensions {
            return Err(Error::Vector(VectorError::DimensionMismatch {
                expected: self.config.dimensions,
                actual: query.len(),
            }));
        }

        let k_capped = k.min(self.max_k);
        if k_capped == 0 {
            return Ok(Vec::new());
        }

        let max_candidates = self.len().min(self.max_k);
        if max_candidates == 0 {
            return Ok(Vec::new());
        }

        // Evaluate user predicates outside the inner index lock to avoid callback/writer deadlocks.
        let mut candidate_k = k_capped.min(max_candidates);
        loop {
            // Convert matches while holding the inner lock so any usearch-backed buffers
            // are not observed after concurrent mutation.
            let candidates =
                {
                    let index = self.inner.read();
                    let mut maybe_matches = None;

                    for attempt in 0..MAX_SEARCH_ATTEMPTS {
                        match index.search(query, candidate_k) {
                            Ok(found) => {
                                maybe_matches = Some(found);
                                break;
                            }
                            Err(e) => {
                                let error_msg = e.to_string();
                                if is_retryable_usearch_error(&error_msg)
                                    && attempt + 1 < MAX_SEARCH_ATTEMPTS
                                {
                                    self.stats.search_retries.fetch_add(1, Ordering::Relaxed);
                                    let delay_ms = 1u64 << attempt;
                                    std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                                    continue;
                                }

                                if attempt > 0 {
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

                    // Convert matches (already sorted) so we can iterate by best similarity first
                    self.convert_matches(maybe_matches.expect(
                        "filtered search retry loop should have returned or produced matches",
                    ))
                };

            let mut filtered = Vec::with_capacity(k_capped.min(candidates.len()));
            for (node_id, similarity) in candidates {
                let _guard = FilterCallbackGuard::new();
                if predicate(&node_id) {
                    filtered.push((node_id, similarity));
                    if filtered.len() == k_capped {
                        break;
                    }
                }
            }

            if filtered.len() >= k_capped || candidate_k == max_candidates {
                self.stats
                    .searches_performed
                    .fetch_add(1, Ordering::Relaxed);
                return Ok(filtered);
            }

            let next_candidate_k = (candidate_k.saturating_mul(2)).min(max_candidates);
            if next_candidate_k == candidate_k {
                self.stats
                    .searches_performed
                    .fetch_add(1, Ordering::Relaxed);
                return Ok(filtered);
            }
            candidate_k = next_candidate_k;
        }
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
        // Check for re-entrant modification during filtered search (prevents deadlock)
        if IN_FILTER_CALLBACK.with(|flag| flag.get()) {
            return Err(Error::Vector(VectorError::IndexError(
                "Cannot save index from within a search_with_filter callback. \
                 This would cause a deadlock due to lock re-entrancy. \
                 Consider saving after the search completes."
                    .to_string(),
            )));
        }

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
    /// Helper to retry usearch operations on transient errors (like thread pool exhaustion).
    fn retry_usearch<F, T, E>(&self, mut op: F, context: &str) -> Result<T>
    where
        F: FnMut() -> std::result::Result<T, E>,
        E: std::fmt::Display,
    {
        for attempt in 0..MAX_SEARCH_ATTEMPTS {
            match op() {
                Ok(val) => return Ok(val),
                Err(e) => {
                    let error_msg = e.to_string();
                    if is_retryable_usearch_error(&error_msg) && attempt + 1 < MAX_SEARCH_ATTEMPTS {
                        self.stats.search_retries.fetch_add(1, Ordering::Relaxed);
                        let delay_ms = 1u64 << attempt;
                        std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                        continue;
                    }
                    if attempt > 0 {
                        self.stats
                            .search_retry_failures
                            .fetch_add(1, Ordering::Relaxed);
                    }
                    return Err(Error::Vector(VectorError::IndexError(format!(
                        "{}: {}",
                        context, e
                    ))));
                }
            }
        }
        unreachable!("Retry loop should always return")
    }

    /// Internal implementation of index saving.
    ///
    /// This method performs the actual blocking I/O operations for saving the index
    /// and its mappings. It is separated from `save()` to allow the latter to use
    /// `tokio::task::block_in_place` when running within a Tokio runtime.
    /// Check and expand capacity if needed.
    ///
    /// This method ensures that the index has enough capacity to accommodate new vectors.
    /// It handles the lock upgrading dance (read -> write -> read) to minimize contention.
    ///
    /// # Arguments
    ///
    /// * `vectors_to_add` - Number of vectors expected to be added (usually 1)
    fn check_and_expand_capacity(&self, vectors_to_add: usize) -> Result<()> {
        #[cfg(test)]
        if TEST_SKIP_CAPACITY_CHECK.load(Ordering::Relaxed) {
            return Ok(());
        }

        // Padding to account for concurrent additions between check and actual add.
        // This prevents a race condition where multiple threads pass the capacity check
        // but then collectively overflow the capacity, causing a crash in usearch.
        const CAPACITY_PADDING: usize = 128;

        // Optimistic check with read lock
        let index = self.inner.read();
        if index.size() + vectors_to_add + CAPACITY_PADDING <= index.capacity() {
            return Ok(());
        }
        drop(index);

        // Acquire write lock to expand capacity
        let index = self.inner.write();

        // Double check capacity under write lock (race condition check)
        if index.size() + vectors_to_add + CAPACITY_PADDING <= index.capacity() {
            return Ok(());
        }

        // Expand capacity
        // Double capacity, minimum 1024
        let new_capacity = (index.capacity() * 2).max(1024);

        // Use retry logic for reserve as well, just in case
        self.retry_usearch(|| index.reserve(new_capacity), "Failed to expand capacity")?;

        Ok(())
    }

    fn save_internal(&self, path: &Path) -> Result<()> {
        // Acquire save_lock (exclusive) to prevent concurrent adds/removes.
        // This ensures consistency between the index snapshot and the ID mappings.
        let _save_guard = self.save_lock.write();

        // Acquire inner write lock to serialize `save` with concurrent searches.
        let index = self.inner.write();

        // Collect mappings while holding the locks.
        // Memory cost: O(N) allocation (~16MB for 1M nodes), acceptable for infrequent save operation.
        let mappings: Vec<(NodeId, u64)> = self
            .id_mapping
            .iter()
            .map(|e| (*e.key(), *e.value()))
            .collect();
        let count = mappings.len();

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
        // Explicit drop to release lock before I/O
        drop(index);
        drop(_save_guard);

        // Save mappings to companion file with integrity checks
        // Format: [MAGIC:4][VERSION:2][DIMS:8][QUANT:1][METRIC:1][COUNT:8][DATA:16*count][CRC32:4]
        let mappings_path = path.with_extension("usearch.mappings");

        // Calculate total size: Magic(4) + Version(1) + Dims(8) + Quant(1) + Metric(1) + Count(8) + Data(count * 16) + CRC(4)
        // Use checked arithmetic to prevent overflow
        let count_size = count
            .checked_mul(16)
            .ok_or_else(|| Error::Vector(VectorError::IndexError("Index too large".to_string())))?;
        let _total_size = count_size
            .checked_add(4 + 1 + 8 + 1 + 1 + 8 + 4)
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
        Self::write_mappings_to_writer(&mut writer, mappings.into_iter(), count, &self.config)
    }

    /// Helper method to stream mappings to a writer with CRC calculation.
    /// Extracted for testability of error paths.
    fn write_mappings_to_writer<W, I>(
        writer: &mut W,
        mappings_iter: I,
        count: usize,
        config: &HnswConfig,
    ) -> Result<()>
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

        // Version 2 fields: Dimensions, Quantization, Metric
        write_and_hash(
            writer,
            &mut hasher,
            &(config.dimensions as u64).to_le_bytes(),
        )?;
        write_and_hash(writer, &mut hasher, &[config.quantization.to_u8()])?;
        write_and_hash(writer, &mut hasher, &[config.metric.to_u8()])?;

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

    /// Validate loaded index metadata against configuration.
    fn validate_metadata(metadata: Option<IndexMetadata>, config: &HnswConfig) -> Result<()> {
        if let Some(meta) = metadata {
            if meta.dimensions > MAX_VECTOR_DIMENSIONS {
                return Err(Error::Vector(VectorError::InvalidVector {
                    reason: format!(
                        "Stored index dimensions {} exceeds maximum allowed {}",
                        meta.dimensions, MAX_VECTOR_DIMENSIONS
                    ),
                }));
            }
            if meta.dimensions != config.dimensions {
                return Err(Error::Vector(VectorError::IndexError(format!(
                    "Index dimension mismatch: expected {}, found {}",
                    config.dimensions, meta.dimensions
                ))));
            }
            if meta.quantization != config.quantization {
                return Err(Error::Vector(VectorError::IndexError(format!(
                    "Index quantization mismatch: expected {:?}, found {:?}",
                    config.quantization, meta.quantization
                ))));
            }
            if meta.metric != config.metric {
                return Err(Error::Vector(VectorError::IndexError(format!(
                    "Index metric mismatch: expected {:?}, found {:?}",
                    config.metric, meta.metric
                ))));
            }
        } else {
            // Legacy index (Version 1)
            // Prevent usage of custom metric with legacy index to avoid buffer over-read vulnerability
            // (since we cannot verify dimensions/quantization)
            if config.custom_metric.is_some() {
                return Err(Error::Vector(VectorError::IndexError(
                    "Cannot use custom metric with legacy index (missing metadata validation)"
                        .to_string(),
                )));
            }
        }
        Ok(())
    }

    /// Convert usearch matches to sorted vector of (NodeId, similarity) tuples.
    ///
    /// This helper function encapsulates the common logic of:
    /// 1. Converting usearch keys back to NodeIds
    /// 2. Converting distances to similarities based on the configured metric
    /// 3. Returns results sorted by similarity (descending order)
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
    /// # Optimization: No Explicit Sort
    ///
    /// We rely on `usearch` returning results sorted by distance (nearest neighbors first).
    /// Since all our supported distance-to-similarity conversions are monotonic decreasing,
    /// sorted distances (ascending) map to sorted similarities (descending).
    ///
    /// Explicit sorting O(k log k) is removed, saving CPU cycles on hot paths.
    ///
    /// # Arguments
    ///
    /// * `matches` - The raw matches from usearch containing keys and distances
    ///
    /// # Returns
    ///
    /// A sorted vector of (NodeId, similarity) pairs where higher similarity means more similar.
    fn convert_matches(&self, matches: Matches) -> Vec<(NodeId, f32)> {
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
                    DistanceMetric::DotProduct => 1.0 - distance,
                    DistanceMetric::Haversine => -distance,
                    DistanceMetric::Hamming => -distance,
                    DistanceMetric::Tanimoto => 1.0 - distance,
                };

                results.push((node_id, similarity));
            }
        }

        results
    }
}

/// Load and verify mappings from a companion file.
/// Returns (id_mapping, reverse_mapping, max_key, metadata) or error if integrity check fails.
/// Format V1: `[MAGIC:4][VERSION:1][COUNT:8][DATA:16*count][CRC32:4]`
/// Format V2: `[MAGIC:4][VERSION:2][DIMS:8][QUANT:1][METRIC:1][COUNT:8][DATA:16*count][CRC32:4]`
#[allow(clippy::type_complexity)]
fn load_mappings_with_integrity(
    mappings_path: &Path,
) -> Result<(
    DashMap<NodeId, u64>,
    DashMap<u64, NodeId>,
    u64,
    Option<IndexMetadata>,
)> {
    let id_mapping = DashMap::new();
    let reverse_mapping = DashMap::new();
    let mut max_key = 0u64;

    if !mappings_path.exists() {
        return Ok((id_mapping, reverse_mapping, max_key, None));
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

    // Minimum size check (V1 min size)
    // Magic(4) + Version(1) + Count(8) + CRC(4) = 17 bytes
    if file_len < 17 {
        return Err(Error::Vector(VectorError::IndexError(
            "Mapping file too small or corrupted".to_string(),
        )));
    }

    let mut reader = std::io::BufReader::new(file);
    let mut hasher = Hasher::new();

    // 1. Read Start of Header (5 bytes: Magic + Version)
    let mut header_start = [0u8; 5];
    reader.read_exact(&mut header_start).map_err(|e| {
        Error::Vector(VectorError::IndexError(format!(
            "Failed to read mappings header start: {}",
            e
        )))
    })?;

    hasher.update(&header_start);

    // Verify magic bytes
    if &header_start[0..4] != MAPPING_MAGIC {
        return Err(Error::Vector(VectorError::IndexError(
            "Invalid mapping file: bad magic bytes".to_string(),
        )));
    }

    let version = header_start[4];

    // Read remaining header based on version
    let (count, metadata, header_overhead) = match version {
        1 => {
            // V1: Count(8)
            let mut buf = [0u8; 8];
            reader.read_exact(&mut buf).map_err(|e| {
                Error::Vector(VectorError::IndexError(format!(
                    "Failed to read V1 header fields: {}",
                    e
                )))
            })?;
            hasher.update(&buf);
            let count = u64::from_le_bytes(buf) as usize;
            // Overhead: Magic(4) + Version(1) + Count(8) + CRC(4) = 17
            (count, None, 17)
        }
        2 => {
            // V2: Dims(8) + Quant(1) + Metric(1) + Count(8)
            let mut buf = [0u8; 18];
            reader.read_exact(&mut buf).map_err(|e| {
                Error::Vector(VectorError::IndexError(format!(
                    "Failed to read V2 header fields: {}",
                    e
                )))
            })?;
            hasher.update(&buf);

            let dims = u64::from_le_bytes(buf[0..8].try_into().unwrap()) as usize;
            let quant = Quantization::from_u8(buf[8])?;
            let metric = DistanceMetric::from_u8(buf[9])?;
            let count = u64::from_le_bytes(buf[10..18].try_into().unwrap()) as usize;

            let meta = IndexMetadata {
                dimensions: dims,
                quantization: quant,
                metric,
            };
            // Overhead: Magic(4) + Version(1) + Dims(8) + Quant(1) + Metric(1) + Count(8) + CRC(4) = 27
            (count, Some(meta), 27)
        }
        v => {
            return Err(Error::Vector(VectorError::IndexError(format!(
                "Unsupported mapping file version: {} (expected 1 or {})",
                v, MAPPING_VERSION
            ))));
        }
    };

    // Security Check: Enforce maximum mappings count to prevent OOM DoS
    if count > MAX_MAPPINGS_COUNT {
        return Err(Error::Vector(VectorError::IndexError(format!(
            "Mappings count {} exceeds maximum allowed {}",
            count, MAX_MAPPINGS_COUNT
        ))));
    }

    // Verify data size with checked arithmetic
    // Cast to u64 for file size comparison
    let data_size = (count as u64).checked_mul(16).ok_or_else(|| {
        Error::Vector(VectorError::IndexError(
            "Mapping count too large (overflow)".to_string(),
        ))
    })?;
    let expected_size = data_size.checked_add(header_overhead).ok_or_else(|| {
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

    Ok((id_mapping, reverse_mapping, max_key, metadata))
}

impl HnswIndex {
    /// Returns the number of ID mappings.
    ///
    /// This is useful for consistency checks against `len()`.
    /// Ideally, `len() == len_mappings()`. If `len() > len_mappings()`,
    /// there are vectors in the index that cannot be retrieved (Zombie Vectors).
    pub fn len_mappings(&self) -> usize {
        self.id_mapping.len()
    }

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
        if config.dimensions > MAX_VECTOR_DIMENSIONS {
            return Err(Error::Vector(VectorError::InvalidVector {
                reason: format!(
                    "dimensions {} exceeds maximum allowed {}",
                    config.dimensions, MAX_VECTOR_DIMENSIONS
                ),
            }));
        }

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

        // Security Check: Verify loaded index dimensions match configuration
        // This prevents buffer over-reads in custom metric wrappers if a malicious actor
        // provides a mismatched index (e.g. index has 4 dims, config has 100).
        if index.dimensions() != config.dimensions {
            return Err(Error::Vector(VectorError::IndexError(format!(
                "Index dimension mismatch: usearch index has {}, config has {}",
                index.dimensions(),
                config.dimensions
            ))));
        }

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
        let (id_mapping, reverse_mapping, max_key, metadata) =
            load_mappings_with_integrity(&mappings_path)?;

        // Validate metadata
        Self::validate_metadata(metadata, &config)?;

        Ok(HnswIndex {
            inner: Arc::new(RwLock::new(index)),
            config,
            id_mapping: Arc::new(id_mapping),
            reverse_mapping: Arc::new(reverse_mapping),
            next_key: AtomicU64::new(max_key + 1),
            stats: Arc::new(IndexStats::default()),
            max_k: MAX_K,
            is_mmap: false,
            save_lock: Arc::new(RwLock::new(())),
            entry_locks: (0..NUM_ENTRY_LOCKS).map(|_| Mutex::new(())).collect(),
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

        if dimensions > MAX_VECTOR_DIMENSIONS {
            return Err(Error::Vector(VectorError::InvalidVector {
                reason: format!(
                    "Memory-mapped index dimensions {} exceeds maximum allowed {}",
                    dimensions, MAX_VECTOR_DIMENSIONS
                ),
            }));
        }

        // Load mappings from companion file with integrity verification
        let mappings_path = path.with_extension("usearch.mappings");
        let (id_mapping, reverse_mapping, max_key, metadata) =
            load_mappings_with_integrity(&mappings_path)?;

        // Restore configuration from metadata if available
        let (quantization, metric) = if let Some(meta) = metadata {
            // Verify dimensions match what usearch reports
            if meta.dimensions != dimensions {
                return Err(Error::Vector(VectorError::IndexError(format!(
                    "Index dimension mismatch: usearch reported {}, metadata says {}",
                    dimensions, meta.dimensions
                ))));
            }
            (meta.quantization, meta.metric)
        } else {
            // Legacy index: fallback to defaults
            (Quantization::default(), DistanceMetric::Cosine)
        };

        Ok(HnswIndex {
            inner: Arc::new(RwLock::new(index)),
            config: HnswConfig {
                dimensions,
                m: connectivity,
                quantization,
                metric,
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
            save_lock: Arc::new(RwLock::new(())),
            entry_locks: (0..NUM_ENTRY_LOCKS).map(|_| Mutex::new(())).collect(),
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
    fn test_metric_wrapper_safe_on_unaligned() {
        let distance_fn = Arc::new(|_: &[f32], _: &[f32]| 0.0);
        let wrapper = create_metric_wrapper(4, distance_fn);

        // Create a buffer and get an unaligned pointer
        let buffer = [0u8; 32];
        // Address + 1 is definitely unaligned for f32 (align 4)
        let unaligned_ptr = unsafe { buffer.as_ptr().add(1) } as *const f32;
        let aligned_vec = [0.0f32; 4];
        let aligned_ptr = aligned_vec.as_ptr();

        let result = wrapper(unaligned_ptr, aligned_ptr);
        assert_eq!(result, f32::MAX);
    }

    #[test]
    fn test_is_retryable_error_matching() {
        assert!(is_retryable_usearch_error(
            "Error: No available threads to lock for search"
        ));
        assert!(!is_retryable_usearch_error("Other error"));
    }

    #[test]
    fn test_hnsw_config_serialization_round_trip() {
        let config = HnswConfig {
            dimensions: 128,
            metric: DistanceMetric::Euclidean,
            m: 32,
            ef_construction: 200,
            ef_search: 100,
            capacity: 5000,
            quantization: Quantization::F16,
            storage: StorageMode::InMemory,
            custom_metric: None,
        };

        let mut buffer = Vec::new();
        config.serialize_into(&mut buffer).unwrap();

        let mut cursor = std::io::Cursor::new(buffer);
        let deserialized = HnswConfig::deserialize_from(&mut cursor).unwrap();

        assert_eq!(config, deserialized);
    }

    #[test]
    fn test_hnsw_config_deserialize_legacy() {
        // Legacy format: missing quantization byte
        let config = HnswConfig {
            dimensions: 128,
            metric: DistanceMetric::Cosine,
            m: 16,
            ef_construction: 128,
            ef_search: 64,
            capacity: 1000,
            quantization: Quantization::F32, // Default
            storage: StorageMode::InMemory,
            custom_metric: None,
        };

        let mut buffer = Vec::new();
        // Manually write legacy format
        buffer.extend_from_slice(&(config.dimensions as u64).to_le_bytes());
        buffer.push(config.metric.to_u8());
        buffer.extend_from_slice(&(config.m as u64).to_le_bytes());
        buffer.extend_from_slice(&(config.ef_construction as u64).to_le_bytes());
        buffer.extend_from_slice(&(config.ef_search as u64).to_le_bytes());
        buffer.extend_from_slice(&(config.capacity as u64).to_le_bytes());
        // STOP here (no quantization byte)

        let mut cursor = std::io::Cursor::new(buffer);
        let deserialized = HnswConfig::deserialize_from(&mut cursor).unwrap();

        assert_eq!(config, deserialized);
        assert_eq!(deserialized.quantization, Quantization::F32); // Check default
    }

    #[test]
    fn test_hnsw_config_deserialize_invalid_metric() {
        let mut buffer = Vec::new();
        buffer.extend_from_slice(&128u64.to_le_bytes()); // dimensions
        buffer.push(99); // Invalid metric
        // rest doesn't matter much as it should fail early, but let's pad it
        buffer.resize(100, 0);

        let mut cursor = std::io::Cursor::new(buffer);
        let result = HnswConfig::deserialize_from(&mut cursor);
        assert!(result.is_err());
    }

    #[test]
    fn test_hnsw_config_deserialize_invalid_quantization() {
        // Construct a buffer that is valid until quantization byte
        let config = HnswConfig::default();
        let mut buffer = Vec::new();
        // Write valid parts manually to ensure we reach quantization read
        buffer.extend_from_slice(&(config.dimensions as u64).to_le_bytes());
        buffer.push(config.metric.to_u8());
        buffer.extend_from_slice(&(config.m as u64).to_le_bytes());
        buffer.extend_from_slice(&(config.ef_construction as u64).to_le_bytes());
        buffer.extend_from_slice(&(config.ef_search as u64).to_le_bytes());
        buffer.extend_from_slice(&(config.capacity as u64).to_le_bytes());

        // Write INVALID quantization byte
        buffer.push(99);

        let mut cursor = std::io::Cursor::new(buffer);
        let result = HnswConfig::deserialize_from(&mut cursor);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Invalid quantization")
        );
    }

    #[test]
    fn test_builder_validation_limits() {
        // M too large
        let res = HnswIndexBuilder::new(10, DistanceMetric::Cosine)
            .m(100)
            .build();
        assert!(res.is_err());

        // M too small
        let res = HnswIndexBuilder::new(10, DistanceMetric::Cosine)
            .m(0)
            .build();
        assert!(res.is_err());

        // Dimensions 0
        let res = HnswIndexBuilder::new(0, DistanceMetric::Cosine).build();
        assert!(res.is_err());
    }

    #[test]
    fn test_custom_metric_safety_check() {
        let result = HnswIndexBuilder::new(128, DistanceMetric::Cosine)
            .quantization(Quantization::I8) // Not F32
            .with_custom_metric("test", |_, _| 0.0)
            .build();

        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("only supported with F32")
        );
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
    fn test_search_results_are_sorted() -> Result<()> {
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .m(16)
            .ef_construction(100)
            .build()?;

        // Add random vectors
        use rand::Rng;
        let mut rng = rand::thread_rng();
        for i in 1..=100 {
            let vec: Vec<f32> = (0..4).map(|_| rng.r#gen()).collect();
            index.add(NodeId::new(i).unwrap(), &vec)?;
        }

        let query: Vec<f32> = (0..4).map(|_| rng.r#gen()).collect();
        let results = index.search(&query, 20)?;

        for i in 0..results.len().saturating_sub(1) {
            assert!(
                results[i].1 >= results[i + 1].1,
                "Results unsorted at index {}: {} < {}",
                i,
                results[i].1,
                results[i + 1].1
            );
        }
        Ok(())
    }

    #[test]
    fn test_dot_product_similarity_metric() -> Result<()> {
        // Test DotProduct similarity conversion
        // Dot Product of (1,0) and (1,0) is 1.0.
        // usearch IP metric returns 1 - dot_product (or similar).
        // We verify that our conversion (1.0 - distance) yields the correct dot product (1.0).

        let index = HnswIndexBuilder::new(2, DistanceMetric::DotProduct).build()?;
        let node = NodeId::new(1).unwrap();
        index.add(node, &[1.0, 0.0])?;

        let results = index.search(&[1.0, 0.0], 1)?;
        assert_eq!(results.len(), 1);
        let similarity = results[0].1;

        // We expect similarity to be exactly 1.0 for normalized dot product of identical unit vectors.
        // Previously this was returning 0.0 (or -0.0) due to incorrect conversion.
        assert!(
            (similarity - 1.0).abs() < 0.001,
            "Expected 1.0, got {}",
            similarity
        );

        Ok(())
    }

    #[test]
    fn test_metric_wrapper_safe_on_unaligned() {
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

        // Pass unaligned pointer - should return f32::MAX
        let result = wrapper(valid_ptr, unaligned_ptr);
        assert_eq!(result, f32::MAX);
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
    fn test_metric_wrapper_safe_on_null() {
        // This test ensures that the metric wrapper correctly detects null pointers
        // and returns MAX distance instead of panicking to prevent UB.
        let distance_fn = Arc::new(|_: &[f32], _: &[f32]| 0.0);
        let wrapper = create_metric_wrapper(4, distance_fn);

        // Create a valid pointer for one argument
        let vec = [0.0f32; 4];
        let valid_ptr = vec.as_ptr();
        let null_ptr = std::ptr::null();

        // Pass null pointer - should return f32::MAX
        let result = wrapper(valid_ptr, null_ptr);
        assert_eq!(result, f32::MAX);
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
        // Modify the node ID part of the data
        // Header V2 size: 4(Magic) + 1(Ver) + 8(Dims) + 1(Quant) + 1(Metric) + 8(Count) = 23 bytes
        let header_size = 23;
        if data.len() > header_size {
            data[header_size] = data[header_size].wrapping_add(1);
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
        // Count is at offset 15 (Magic 4 + Version 1 + Dims 8 + Quant 1 + Metric 1)
        // Original count is 1. Let's make it 2.
        let count_offset = 15;
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
        let count_offset = 15; // V2 offset
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
                    msg.contains("overflow") || msg.contains("exceeds maximum allowed"),
                    "Expected overflow or max limit error, got: {}",
                    msg
                );
            }
            Ok(_) => panic!("Expected IndexError with overflow/limit message, got: Ok(_)"),
            Err(e) => panic!(
                "Expected IndexError with overflow message, got: Err({:?})",
                e
            ),
        }
        Ok(())
    }

    #[test]
    fn test_load_mappings_count_limit() -> Result<()> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_index.usearch");
        let mappings_path = path.with_extension("usearch.mappings");

        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;
        index.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0])?;
        index.save(&path)?;

        // Modify count to be MAX_MAPPINGS_COUNT + 1
        let mut data = std::fs::read(&mappings_path).unwrap();
        let count_offset = 15; // V2 offset
        let huge_count = (super::MAX_MAPPINGS_COUNT + 1) as u64;

        // Write huge count
        let count_bytes = huge_count.to_le_bytes();
        data[count_offset..count_offset + 8].copy_from_slice(&count_bytes);

        // Update CRC
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
                    msg.contains("exceeds maximum allowed"),
                    "Expected max limit error, got: {}",
                    msg
                );
            }
            Ok(_) => panic!("Expected IndexError with limit message, got: Ok(_)"),
            Err(e) => panic!("Expected IndexError with limit message, got: Err({:?})", e),
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

        let config = HnswConfig::default();

        // Case 1: Fail during header (MAGIC)
        // Magic is 4 bytes. Fail at byte 3.
        let mut writer = MockFailWriter::new(3);
        let result = HnswIndex::write_mappings_to_writer(
            &mut writer,
            mappings.iter().copied(),
            mappings.len(),
            &config,
        );
        assert!(result.is_err());
        if let Err(Error::Vector(VectorError::IndexError(msg))) = result {
            assert!(msg.contains("Failed to write mappings"));
        } else {
            panic!("Expected IndexError");
        }

        // Case 2: Fail during data writing
        // V2 Header: Magic(4) + Version(1) + Dims(8) + Quant(1) + Metric(1) + Count(8) = 23 bytes
        // Data is 16 bytes per item.
        // Fail after header + 1st item (16 bytes) + 1 byte
        let mut writer = MockFailWriter::new(23 + 16 + 1);
        let result = HnswIndex::write_mappings_to_writer(
            &mut writer,
            mappings.iter().copied(),
            mappings.len(),
            &config,
        );
        assert!(result.is_err());
        if let Err(Error::Vector(VectorError::IndexError(msg))) = result {
            assert!(msg.contains("Failed to write mappings"));
        }

        // Case 3: Fail during CRC writing
        // Total data size = 23 + 32 = 55 bytes.
        // CRC is 4 bytes.
        // Fail at 55 + 1 byte (during CRC write)
        let mut writer = MockFailWriter::new(55 + 1);
        let result = HnswIndex::write_mappings_to_writer(
            &mut writer,
            mappings.iter().copied(),
            mappings.len(),
            &config,
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
        let config = HnswConfig::default();
        let mut writer = MockFlushFailWriter;
        let result = HnswIndex::write_mappings_to_writer(
            &mut writer,
            mappings.iter().copied(),
            mappings.len(),
            &config,
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
    fn test_custom_metric_execution_coverage() {
        // This test ensures that the custom metric wrapper and its guard logic are executed,
        // satisfying code coverage requirements for the new lines added in create_metric_wrapper.
        let metric_fn = |a: &[f32], b: &[f32]| -> f32 {
            a.iter().zip(b.iter()).map(|(x, y)| (x - y).abs()).sum()
        };

        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .quantization(Quantization::F32) // Required for custom metric
            .with_custom_metric("manhattan", metric_fn)
            .build()
            .unwrap();

        // Add more nodes to ensure we trigger enough comparisons to hit the callback
        for i in 0..10 {
            let id = NodeId::new(i + 1).unwrap();
            // Alternate vectors to create some diversity
            let vec = if i % 2 == 0 {
                [1.0, 0.0, 0.0, 0.0]
            } else {
                [0.0, 1.0, 0.0, 0.0]
            };
            index.add(id, &vec).unwrap();
        }

        // Perform search to trigger the metric execution
        // Search for k=5 to force more comparisons
        let results = index.search(&[0.9, 0.1, 0.0, 0.0], 5).unwrap();
        assert_eq!(results.len(), 5);
    }

    #[test]
    fn test_add_race_retry_value_change_coverage() {
        // Test that if another thread changes the mapping (value changed) while we are in add(),
        // we detect it and return a retryable error.
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap();
        let node = NodeId::new(1).unwrap();

        // 1. Add initial vector so we hit Occupied path
        index.add(node, &[1.0, 0.0, 0.0, 0.0]).unwrap();

        // 2. Set hook to change the mapping ID
        TEST_RACE_HOOK.with(|h| {
            h.set(Some(|idx, node_id| {
                // Simulate another thread updating this node to a new key
                // We just manually update the map here.
                // Note: In real life, add() would do this via next_key, but here we just hack the map.
                idx.id_mapping.insert(node_id, 999);
                // Also update reverse mapping to keep index consistent (though add() logic only checks id_mapping)
                idx.reverse_mapping.insert(999, node_id);
            }))
        });

        // 3. Call add() again. It should hit Occupied path, trigger hook, fail validation.
        let result = index.add(node, &[0.0, 1.0, 0.0, 0.0]);

        // 4. Cleanup hook
        TEST_RACE_HOOK.with(|h| h.set(None));

        // 5. Assert error
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::IndexError(msg))) => {
                assert!(msg.contains("Concurrent modification detected"));
                assert!(msg.contains("mapping changed"));
            }
            _ => panic!("Expected concurrent modification error, got {:?}", result),
        }
    }

    #[test]
    fn test_add_race_retry_removal_coverage() {
        // Test that if another thread removes the mapping while we are in add(),
        // we detect it and return a retryable error.
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap();
        let node = NodeId::new(2).unwrap();

        // 1. Add initial vector
        index.add(node, &[1.0, 0.0, 0.0, 0.0]).unwrap();

        // 2. Set hook to remove the mapping
        TEST_RACE_HOOK.with(|h| {
            h.set(Some(|idx, node_id| {
                idx.id_mapping.remove(&node_id);
            }))
        });

        // 3. Call add() again
        let result = index.add(node, &[0.0, 1.0, 0.0, 0.0]);

        // 4. Cleanup
        TEST_RACE_HOOK.with(|h| h.set(None));

        // 5. Assert error
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::IndexError(msg))) => {
                assert!(msg.contains("Concurrent modification detected"));
                assert!(msg.contains("node removed"));
            }
            _ => panic!("Expected concurrent modification error, got {:?}", result),
        }
    }

    #[test]
    fn test_add_race_vacant_coverage() {
        // Test the race condition in the Vacant path:
        // We checked map (Vacant), acquired inner lock, added to inner.
        // Then we check map again. If it's Occupied (race!), we must rollback.

        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap();
        let node = NodeId::new(3).unwrap();

        // 1. Set hook to insert the mapping (simulating another thread winning the race)
        TEST_RACE_HOOK.with(|h| {
            h.set(Some(|idx, node_id| {
                // Insert a dummy key to simulate another thread claiming this ID
                idx.id_mapping.insert(node_id, 999);
                // Note: We don't strictly need to add to inner/reverse for this test,
                // as add() only checks id_mapping.
            }))
        });

        // 2. Call add(). This uses the Vacant path because the node is new.
        // It should trigger the hook, detect the race, rollback, and fail.
        let result = index.add(node, &[0.5, 0.5, 0.5, 0.5]);

        // 3. Cleanup
        TEST_RACE_HOOK.with(|h| h.set(None));

        // 4. Assert error
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::IndexError(msg))) => {
                assert!(msg.contains("Concurrent add detected"));
                assert!(msg.contains("vector already exists"));
            }
            _ => panic!("Expected concurrent add error, got {:?}", result),
        }

        // 5. Verify rollback: The vector we tried to add should NOT be in the index.
        // The index should be empty (except for potentially what the hook added, but hook only added mapping)
        // Since hook didn't add to inner, inner size should be 0.
        // Wait, add() adds to inner *before* hook. Then rollback removes it.
        // So inner size should be 0.
        assert_eq!(index.len(), 0);
    }

    #[test]
    fn test_save_coverage() -> Result<()> {
        // Basic save test to ensure save_internal lines are covered
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("coverage.index");

        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;
        index.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0])?;

        // This triggers save_internal
        index.save(&path)?;

        assert!(path.exists());
        Ok(())
    }

    #[test]
    fn test_metric_conversions_accuracy() -> Result<()> {
        // Test all supported metrics to ensure correct distance-to-similarity conversion
        let metrics = vec![
            (DistanceMetric::Cosine, vec![1.0, 0.0], vec![1.0, 0.0], 1.0),   // Identical -> 1.0
            (DistanceMetric::Cosine, vec![1.0, 0.0], vec![0.0, 1.0], 0.0),   // Orthogonal -> 0.0
            (DistanceMetric::Euclidean, vec![0.0, 0.0], vec![0.0, 0.0], -0.0), // Identical -> 0 (negated)
            (DistanceMetric::Euclidean, vec![0.0, 0.0], vec![3.0, 4.0], -25.0), // Dist=5^2=25 -> -25
            (DistanceMetric::DotProduct, vec![1.0, 0.0], vec![1.0, 0.0], 1.0), // Dot=1 -> 1
        ];

        for (metric, v1, v2, expected) in metrics {
            let index = HnswIndexBuilder::new(v1.len(), metric).build()?;
            let n1 = NodeId::new(1).unwrap();
            index.add(n1, &v1)?;
            let results = index.search(&v2, 1)?;
            assert_eq!(results.len(), 1);
            let sim = results[0].1;
            assert!((sim - expected).abs() < 0.001, "Metric {:?} failed. Expected {}, got {}", metric, expected, sim);
        }

        // Property checks for others
        // Note: Tanimoto excluded due to upstream usearch issue returning NaN for F32 vectors
        let other_metrics = vec![DistanceMetric::Haversine, DistanceMetric::Hamming];
        for metric in other_metrics {
             let index = HnswIndexBuilder::new(2, metric).build()?;
             let n1 = NodeId::new(1).unwrap();

             // Use binary-like vectors for Hamming/Tanimoto safety, avoiding potential 0/0 NaNs in Tanimoto
             let v = vec![1.0, 0.0];
             index.add(n1, &v)?;
             let results = index.search(&v, 1)?;
             let sim = results[0].1;

             // Identical vectors should have "max" similarity.
             // For distance-based (Haversine, Hamming), distance is 0, so similarity should be -0 = 0.
             // For Tanimoto, distance is 0, similarity is 1-0 = 1.
             let expected_max = match metric {
                 DistanceMetric::Tanimoto => 1.0,
                 _ => 0.0, // -0.0
             };

             if sim.is_nan() {
                 panic!("Metric {:?} returned NaN similarity for vector {:?}", metric, v);
             }

             assert!((sim - expected_max).abs() < 0.001, "Metric {:?} identity check failed. Expected {}, got {}", metric, expected_max, sim);
        }

        Ok(())
    }

    #[test]
    fn test_filter_rejects_all() -> Result<()> {
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;
        for i in 1..=10 {
            let id = NodeId::new(i).unwrap();
            index.add(id, &[1.0, 0.0, 0.0, 0.0])?;
        }

        // Filter that rejects everything
        let results = index.search_with_filter(&[1.0, 0.0, 0.0, 0.0], 10, |_| false)?;
        assert_eq!(results.len(), 0);

        Ok(())
    }

    #[test]
    fn test_quantization_reduces_index_size() -> Result<()> {
        let dir = tempfile::tempdir().unwrap();
        let f32_path = dir.path().join("f32.index");
        let i8_path = dir.path().join("i8.index");

        let dimensions = 128;
        let count = 200; // Small count for unit test speed, but enough to see diff

        // 1. Build F32 Index
        {
            let index = HnswIndexBuilder::new(dimensions, DistanceMetric::Cosine)
                .quantization(Quantization::F32)
                .build()?;

            for i in 0..count {
                let vec = vec![0.1; dimensions];
                index.add(NodeId::new(i as u64 + 1).unwrap(), &vec)?;
            }
            index.save(&f32_path)?;
        }

        // 2. Build I8 Index
        {
            let index = HnswIndexBuilder::new(dimensions, DistanceMetric::Cosine)
                .quantization(Quantization::I8)
                .build()?;

            for i in 0..count {
                let vec = vec![0.1; dimensions];
                index.add(NodeId::new(i as u64 + 1).unwrap(), &vec)?;
            }
            index.save(&i8_path)?;
        }

        let f32_size = std::fs::metadata(&f32_path).unwrap().len();
        let i8_size = std::fs::metadata(&i8_path).unwrap().len();

        // I8 should be significantly smaller
        assert!(i8_size < f32_size, "I8 index ({} bytes) should be smaller than F32 index ({} bytes)", i8_size, f32_size);

        Ok(())
    }
}

#[cfg(test)]
mod warden_tests {
    use super::*;
    use crate::core::property::MAX_VECTOR_DIMENSIONS;

    #[test]
    fn test_config_deserialize_dimensions_too_large() {
        let huge_dims = (MAX_VECTOR_DIMENSIONS + 1) as u64;
        let mut buffer = Vec::new();
        buffer.extend_from_slice(&huge_dims.to_le_bytes()); // dimensions
        // Add minimal remaining fields to avoid early EOF if we got past dimensions check
        buffer.push(0); // metric
        buffer.extend_from_slice(&16u64.to_le_bytes()); // m
        buffer.extend_from_slice(&128u64.to_le_bytes()); // ef_construction
        buffer.extend_from_slice(&64u64.to_le_bytes()); // ef_search
        buffer.extend_from_slice(&1000u64.to_le_bytes()); // capacity
        buffer.push(0); // quantization

        let mut cursor = std::io::Cursor::new(buffer);
        let result = HnswConfig::deserialize_from(&mut cursor);

        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("dimensions"));
        assert!(msg.contains("exceeds maximum allowed"));
    }

    #[test]
    fn test_validate_metadata_dimensions_too_large() {
        let huge_dims = MAX_VECTOR_DIMENSIONS + 1;
        let metadata = Some(IndexMetadata {
            dimensions: huge_dims,
            quantization: Quantization::F32,
            metric: DistanceMetric::Cosine,
        });
        let config = HnswConfig::default();

        let result = HnswIndex::validate_metadata(metadata, &config);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("Stored index dimensions"));
        assert!(msg.contains("exceeds maximum allowed"));
    }

    #[test]
    fn test_load_dimensions_too_large_in_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.index");

        // Create a config with manually set huge dimensions (bypassing builder)
        let config = HnswConfig {
            dimensions: MAX_VECTOR_DIMENSIONS + 1,
            ..Default::default()
        };

        // Attempt to load (file existence doesn't matter as config check is first)
        let result = HnswIndex::load(&path, config);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("dimensions"));
        assert!(msg.contains("exceeds maximum allowed"));
    }

    #[test]
    fn test_open_mmap_dimensions_too_large_in_metadata() {
        // This is harder to test without mocking the usearch FFI or creating a file.
        // But we can test the load_mappings_with_integrity part via HnswIndex::load if we forge a mappings file.
        // HnswIndex::open_mmap first calls Index::new(), then index.view().
        // If we can't easily mock usearch, maybe we can rely on unit testing `validate_metadata` which we did above.
        // However, `open_mmap` calls `index.dimensions()` which comes from C++.
        //
        // Let's rely on `test_validate_metadata_dimensions_too_large` covering the core logic,
        // and verify `open_mmap`'s check via a mocked scenario if possible, or just accept that `validate_metadata` is the shared logic.
        //
        // Wait, `open_mmap` has its own explicit check:
        // if dimensions > MAX_VECTOR_DIMENSIONS { ... }
        //
        // To trigger this, `index.dimensions()` must return a huge value.
        // We can't force `usearch::Index` to report a huge dimension without a valid file.
        // But we can skip this one if we are confident, OR we can try to trick it.
        //
        // Actually, we can test `load_mappings_with_integrity` logic via `validate_metadata` test.
        // The check in `open_mmap` covers the case where the *binary index file* itself reports huge dimensions.
        // Since `usearch` is a black box, we can't easily generate such a file without `HnswIndexBuilder` (which forbids it).
        //
        // We will stick to testing the accessible Rust parts.
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;

    #[test]
    fn test_metric_wrapper_null_pointer() {
        let distance_fn = Arc::new(|_: &[f32], _: &[f32]| 0.0);
        let wrapper = create_metric_wrapper(4, distance_fn);

        let null_ptr: *const f32 = std::ptr::null();
        let valid_data = [0.0f32; 4];
        let valid_ptr = valid_data.as_ptr();

        // This should return f32::MAX
        let result = wrapper(null_ptr, valid_ptr);
        assert_eq!(result, f32::MAX);
    }

    #[test]
    fn test_metric_wrapper_unaligned_pointer() {
        let distance_fn = Arc::new(|_: &[f32], _: &[f32]| 0.0);
        let wrapper = create_metric_wrapper(4, distance_fn);

        let data = [0u8; 32];
        let unaligned_ptr = unsafe { data.as_ptr().add(1) as *const f32 };
        let valid_data = [0.0f32; 4];
        let valid_ptr = valid_data.as_ptr();

        // This should return f32::MAX
        let result = wrapper(unaligned_ptr, valid_ptr);
        assert_eq!(result, f32::MAX);
    }

    #[test]
    fn test_filter_callback_guard_reset() {
        // Ensure flag is initially false
        IN_FILTER_CALLBACK.with(|flag| flag.set(false));

        {
            let _guard = FilterCallbackGuard::new();
            // Verify flag is set to true
            assert!(IN_FILTER_CALLBACK.with(|flag| flag.get()));
        }

        // Verify flag is reset to false after drop
        assert!(!IN_FILTER_CALLBACK.with(|flag| flag.get()));
    }

    #[test]
    fn test_filter_callback_guard_manual_drop() {
        IN_FILTER_CALLBACK.with(|flag| flag.set(false));

        let guard = FilterCallbackGuard::new();
        assert!(IN_FILTER_CALLBACK.with(|flag| flag.get()));

        drop(guard);
        assert!(!IN_FILTER_CALLBACK.with(|flag| flag.get()));
    }

    #[test]
    fn test_metric_wrapper_panic_resilience() {
        // Ensure that a panicking metric function doesn't crash the process
        // but returns f32::MAX instead.
        let distance_fn = Arc::new(|_: &[f32], _: &[f32]| -> f32 {
            panic!("Test panic");
        });
        let wrapper = create_metric_wrapper(4, distance_fn);

        let data = [0.0f32; 4];
        let ptr = data.as_ptr();

        // This should NOT panic
        let result = wrapper(ptr, ptr);

        // Should return max distance
        assert_eq!(result, f32::MAX);
    }

    #[test]
    fn test_metric_wrapper_success_direct() {
        // Ensure that a non-panicking metric function works correctly.
        // This provides direct coverage of the Ok path in catch_unwind.
        let distance_fn = Arc::new(|a: &[f32], b: &[f32]| -> f32 {
            a.iter().zip(b.iter()).map(|(x, y)| (x - y).abs()).sum()
        });
        let wrapper = create_metric_wrapper(4, distance_fn);

        let data_a = [1.0f32, 2.0, 3.0, 4.0];
        let data_b = [1.5f32, 2.5, 3.5, 4.5];

        let result = wrapper(data_a.as_ptr(), data_b.as_ptr());

        // |1.0-1.5| + |2.0-2.5| + |3.0-3.5| + |4.0-4.5| = 0.5 * 4 = 2.0
        assert!((result - 2.0).abs() < f32::EPSILON);
    }
}

#[cfg(test)]
mod capacity_tests {
    use super::*;

    #[test]
    fn test_capacity_check_and_expand() {
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .initial_capacity(10)
            .build()
            .unwrap();

        // Initially size 0, capacity 10
        assert_eq!(index.len(), 0);

        // This should pass without expanding
        index.check_and_expand_capacity(1).unwrap();

        // Fill to capacity
        for i in 0..10 {
            let id = NodeId::new(i + 1).unwrap();
            index.add(id, &[1.0, 0.0, 0.0, 0.0]).unwrap();
        }

        assert_eq!(index.len(), 10);

        // This should trigger expansion
        // Note: we can't easily assert on capacity here without exposing internal usearch state
        // but we can verify it doesn't error
        index.check_and_expand_capacity(1).unwrap();
    }
}

#[cfg(test)]
mod race_recovery_tests {
    use super::*;

    #[test]
    fn test_vacant_path_race_recovery() -> Result<()> {
        // Setup: Ensure we skip the optimistic check
        TEST_SKIP_CAPACITY_CHECK.store(true, Ordering::SeqCst);
        struct ResetGuard;
        impl Drop for ResetGuard {
            fn drop(&mut self) {
                TEST_SKIP_CAPACITY_CHECK.store(false, Ordering::SeqCst);
            }
        }
        let _reset = ResetGuard;

        // Create index with small capacity
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .initial_capacity(10)
            .build()?;

        // Fill to capacity
        for i in 0..10 {
            index.add(NodeId::new(i + 1).unwrap(), &[1.0, 0.0, 0.0, 0.0])?;
        }
        assert_eq!(index.len(), 10);

        // At this point, size=10, capacity=10.
        // optimistic check is skipped.
        // add(11) will enter Vacant path.
        // It will acquire inner write lock.
        // It will check size >= capacity (10 >= 10). True.
        // It should trigger expansion.

        index.add(NodeId::new(11).unwrap(), &[1.0, 0.0, 0.0, 0.0])?;

        assert_eq!(index.len(), 11);
        // Verify capacity expanded
        assert!(index.inner.read().capacity() > 10);

        Ok(())
    }

    #[test]
    fn test_occupied_path_inconsistency_race_recovery() -> Result<()> {
        // Setup: Ensure we skip the optimistic check
        TEST_SKIP_CAPACITY_CHECK.store(true, Ordering::SeqCst);
        struct ResetGuard;
        impl Drop for ResetGuard {
            fn drop(&mut self) {
                TEST_SKIP_CAPACITY_CHECK.store(false, Ordering::SeqCst);
            }
        }
        let _reset = ResetGuard;

        // Create index
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .initial_capacity(10)
            .build()?;

        // Fill to capacity
        for i in 0..10 {
            index.add(NodeId::new(i + 1).unwrap(), &[1.0, 0.0, 0.0, 0.0])?;
        }
        assert_eq!(index.len(), 10);

        // We want to trigger the Occupied path logic:
        // 1. Update an existing node (e.g., node 1).
        // 2. Use hook to make inner inconsistent (remove key 1 from inner).
        // 3. Ensure index is full (add dummy key to inner).

        let node_id = NodeId::new(1).unwrap();

        TEST_RACE_HOOK.with(|h| {
            h.set(Some(|idx, _id| {
                // Hook runs after acquiring map lock, before inner lock.
                // We need to modify inner state.
                let index = idx.inner.write();

                // Key for node 1 should be 0 (since it was first added)
                // Remove it from usearch
                // Note: remove in usearch frees up the slot?
                // Wait, usearch remove marks as deleted or actually removes?
                // For dense index, it might just mark it?
                // If it marks it, size might not decrease?
                // Let's check size.
                let _ = index.remove(0); // Removing key 0 (node 1)

                // Now add a dummy key to fill the capacity back up
                // We need a key that doesn't conflict with map.
                // Map has keys 0..9.
                // We removed 0.
                // We add key 999.
                let _ = index.add(999, &[0.0, 1.0, 0.0, 0.0]);

                // Now size should be 10 again.
                // And key 0 is missing from inner.
            }))
        });

        // Trigger update
        // add(1) -> Map Occupied -> Hook runs -> Inner write lock
        // Inner contains(0)? False (removed in hook).
        // Enters else block.
        // Checks size >= capacity. 10 >= 10. True.
        // Expands.
        // Adds key 0.

        index.add(node_id, &[0.0, 1.0, 0.0, 0.0])?;

        // Cleanup
        TEST_RACE_HOOK.with(|h| h.set(None));

        assert_eq!(index.len(), 11); // 9 original + 1 dummy + 1 re-added node 1
        assert!(index.inner.read().capacity() > 10);

        Ok(())
    }
}
