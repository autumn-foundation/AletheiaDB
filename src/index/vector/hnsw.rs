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
use crate::core::property::MAX_VECTOR_DIMENSIONS;
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
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
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

        // Initialize atomic trackers
        let current_size = AtomicUsize::new(index.size());
        let current_ef_search = AtomicUsize::new(index.expansion_search());

        Ok(HnswIndex {
            inner: Arc::new(RwLock::new(index)),
            config: self.config,
            id_mapping: Arc::new(DashMap::new()),
            reverse_mapping: Arc::new(DashMap::new()),
            next_key: AtomicU64::new(0),
            stats: Arc::new(IndexStats::default()),
            max_k: MAX_K,
            is_mmap: false,
            current_size,
            current_ef_search,
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
    /// Current size of the index (shadowed for lock-free access to prevent deadlocks)
    current_size: AtomicUsize,
    /// Current ef_search parameter (shadowed for lock-free access)
    current_ef_search: AtomicUsize,
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

                // Acquire inner write lock
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
                    index.remove(existing_key).map_err(|e| {
                        Error::Vector(VectorError::IndexError(format!(
                            "Failed to remove existing vector: {}",
                            e
                        )))
                    })?;
                    // Decrement shadowed size
                    self.current_size.fetch_sub(1, Ordering::Relaxed);
                }
                // Note: If key doesn't exist, we skip remove() and proceed directly to add()
                // This is safe because add() with a non-existent key will succeed

                // Keep lock held - check if we need to expand capacity
                if index.size() >= index.capacity() {
                    // Double capacity, minimum 1024
                    let new_capacity = (index.capacity() * 2).max(1024);
                    index.reserve(new_capacity).map_err(|e| {
                        Error::Vector(VectorError::IndexError(format!(
                            "Failed to expand capacity: {}",
                            e
                        )))
                    })?;
                }

                // Add the new vector while still holding the lock
                index.add(existing_key, vector).map_err(|e| {
                    Error::Vector(VectorError::IndexError(format!(
                        "Failed to add vector: {}",
                        e
                    )))
                })?;
                // Increment shadowed size
                self.current_size.fetch_add(1, Ordering::Relaxed);

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

                // Step 2: Acquire inner write lock FIRST (follows lock ordering invariant)
                // This prevents deadlock with search_with_filter which holds inner -> dashmap.
                let index = self.inner.write();

                // Check if we need to expand capacity
                if index.size() >= index.capacity() {
                    // Double capacity, minimum 1024
                    let new_capacity = (index.capacity() * 2).max(1024);
                    index.reserve(new_capacity).map_err(|e| {
                        Error::Vector(VectorError::IndexError(format!(
                            "Failed to expand capacity: {}",
                            e
                        )))
                    })?;
                }

                // Step 3: Add to inner usearch index while holding write lock
                index.add(key, vector).map_err(|e| {
                    Error::Vector(VectorError::IndexError(format!(
                        "Failed to add vector: {}",
                        e
                    )))
                })?;
                // Increment shadowed size
                self.current_size.fetch_add(1, Ordering::Relaxed);

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
                    index.remove(key).map_err(|e| {
                        Error::Vector(VectorError::IndexError(format!(
                            "Failed to rollback vector after concurrent add: {}",
                            e
                        )))
                    })?;
                    // Decrement shadowed size
                    self.current_size.fetch_sub(1, Ordering::Relaxed);

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
            // Decrement shadowed size
            self.current_size.fetch_sub(1, Ordering::Relaxed);

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
        // Use shadowed atomic variable to allow lock-free access.
        // This prevents deadlock when called from inside a search_with_filter callback
        // (where a read lock is already held, and a pending writer might block re-entry).
        self.current_size.load(Ordering::Relaxed)
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
        // Check if we are inside a callback where a read lock is already held.
        // If so, acquiring the lock again might deadlock if there's a pending writer.
        if IN_FILTER_CALLBACK.with(|flag| flag.get()) {
            // Safe fallback: return 0.
            // While inaccurate, this is better than crashing or hanging.
            return 0;
        }
        self.inner.read().memory_usage()
    }

    fn quantization(&self) -> Quantization {
        self.config.quantization
    }

    fn compact(&self) -> Result<()> {
        if IN_FILTER_CALLBACK.with(|flag| flag.get()) {
            return Err(Error::Vector(VectorError::IndexError(
                "Cannot compact index from within a search_with_filter callback.".to_string(),
            )));
        }
        // usearch native deletes don't require compaction
        Ok(())
    }
}

// Private helper methods for HnswIndex
impl HnswIndex {
    // ... (save_internal and friends remain unchanged)
    /// Internal implementation of index saving.
    fn save_internal(&self, path: &Path) -> Result<()> {
        // Acquire inner read lock first to ensure consistency between index and mappings.
        // This prevents "Zombie Vectors" where a vector is added to the index but missed in the mappings snapshot.
        let index = self.inner.read();

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

        // Save mappings to companion file
        let mappings_path = path.with_extension("usearch.mappings");

        // Calculate total size
        let count_size = count
            .checked_mul(16)
            .ok_or_else(|| Error::Vector(VectorError::IndexError("Index too large".to_string())))?;
        let _total_size = count_size
            .checked_add(4 + 1 + 8 + 1 + 1 + 8 + 4)
            .ok_or_else(|| Error::Vector(VectorError::IndexError("Index too large".to_string())))?;

        let file = File::create(&mappings_path).map_err(|e| {
            Error::Vector(VectorError::IndexError(format!(
                "Failed to create mappings file: {}",
                e
            )))
        })?;
        let mut writer = BufWriter::new(file);

        Self::write_mappings_to_writer(&mut writer, mappings.into_iter(), count, &self.config)
    }

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

        write_and_hash(writer, &mut hasher, MAPPING_MAGIC)?;
        write_and_hash(writer, &mut hasher, &[MAPPING_VERSION])?;

        write_and_hash(
            writer,
            &mut hasher,
            &(config.dimensions as u64).to_le_bytes(),
        )?;
        write_and_hash(writer, &mut hasher, &[config.quantization.to_u8()])?;
        write_and_hash(writer, &mut hasher, &[config.metric.to_u8()])?;

        write_and_hash(writer, &mut hasher, &count_u64.to_le_bytes())?;

        for (node_id, key) in mappings_iter {
            write_and_hash(writer, &mut hasher, &node_id.as_u64().to_le_bytes())?;
            write_and_hash(writer, &mut hasher, &key.to_le_bytes())?;
        }

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
            if config.custom_metric.is_some() {
                return Err(Error::Vector(VectorError::IndexError(
                    "Cannot use custom metric with legacy index (missing metadata validation)"
                        .to_string(),
                )));
            }
        }
        Ok(())
    }

    fn convert_and_sort_matches(&self, matches: Matches) -> Vec<(NodeId, f32)> {
        let mut results: Vec<(NodeId, f32)> = Vec::with_capacity(matches.keys.len());

        for (key, distance) in matches.keys.iter().zip(matches.distances.iter()) {
            if let Some(node_id_ref) = self.reverse_mapping.get(key) {
                let node_id = *node_id_ref.value();
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
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        results
    }
}

// ... load_mappings_with_integrity remains unchanged ...
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

    if file_len < 17 {
        return Err(Error::Vector(VectorError::IndexError(
            "Mapping file too small or corrupted".to_string(),
        )));
    }

    let mut reader = std::io::BufReader::new(file);
    let mut hasher = Hasher::new();

    let mut header_start = [0u8; 5];
    reader.read_exact(&mut header_start).map_err(|e| {
        Error::Vector(VectorError::IndexError(format!(
            "Failed to read mappings header start: {}",
            e
        )))
    })?;

    hasher.update(&header_start);

    if &header_start[0..4] != MAPPING_MAGIC {
        return Err(Error::Vector(VectorError::IndexError(
            "Invalid mapping file: bad magic bytes".to_string(),
        )));
    }

    let version = header_start[4];

    let (count, metadata, header_overhead) = match version {
        1 => {
            let mut buf = [0u8; 8];
            reader.read_exact(&mut buf).map_err(|e| {
                Error::Vector(VectorError::IndexError(format!(
                    "Failed to read V1 header fields: {}",
                    e
                )))
            })?;
            hasher.update(&buf);
            let count = u64::from_le_bytes(buf) as usize;
            (count, None, 17)
        }
        2 => {
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
            (count, Some(meta), 27)
        }
        v => {
            return Err(Error::Vector(VectorError::IndexError(format!(
                "Unsupported mapping file version: {} (expected 1 or {})",
                v, MAPPING_VERSION
            ))));
        }
    };

    if count > MAX_MAPPINGS_COUNT {
        return Err(Error::Vector(VectorError::IndexError(format!(
            "Mappings count {} exceeds maximum allowed {}",
            count, MAX_MAPPINGS_COUNT
        ))));
    }

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

    if file_len != expected_size {
        return Err(Error::Vector(VectorError::IndexError(format!(
            "Mapping file size mismatch: expected {} bytes, got {}",
            expected_size, file_len
        ))));
    }

    const CHUNK_SIZE: usize = 1024 * 16;
    let mut buffer = vec![0u8; CHUNK_SIZE];
    let mut remaining_entries = count;

    while remaining_entries > 0 {
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
        self.current_ef_search.store(ef_search, Ordering::Relaxed);
    }

    /// Gets the current ef_search value.
    ///
    /// Note: Returns the runtime value which may differ from config if
    /// `set_ef_search` was called.
    pub fn get_ef_search(&self) -> usize {
        // Use shadowed atomic variable to allow lock-free access.
        self.current_ef_search.load(Ordering::Relaxed)
    }

    /// Returns the configuration used to create this index.
    pub fn config(&self) -> HnswConfig {
        self.config.clone()
    }

    /// Returns the M parameter (connections per node).
    pub fn m(&self) -> usize {
        self.config.m
    }

    pub(crate) fn get_id_mappings(&self) -> Vec<(u64, u64)> {
        self.id_mapping
            .iter()
            .map(|entry| (entry.key().as_u64(), *entry.value()))
            .collect()
    }

    pub(crate) fn restore_mapping(&self, node_id: crate::core::id::NodeId, usearch_key: u64) {
        self.id_mapping.insert(node_id, usearch_key);
        self.reverse_mapping.insert(usearch_key, node_id);
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

        if let Some(ref custom) = config.custom_metric {
            let dims = config.dimensions;
            let distance_fn = Arc::clone(&custom.distance_fn);
            let metric_wrapper = create_metric_wrapper(dims, distance_fn);
            index.change_metric(metric_wrapper);
        }

        let mappings_path = path.with_extension("usearch.mappings");
        let (id_mapping, reverse_mapping, max_key, metadata) =
            load_mappings_with_integrity(&mappings_path)?;

        Self::validate_metadata(metadata, &config)?;

        // Initialize atomic trackers
        let current_size = AtomicUsize::new(index.size());
        let current_ef_search = AtomicUsize::new(index.expansion_search());

        Ok(HnswIndex {
            inner: Arc::new(RwLock::new(index)),
            config,
            id_mapping: Arc::new(id_mapping),
            reverse_mapping: Arc::new(reverse_mapping),
            next_key: AtomicU64::new(max_key + 1),
            stats: Arc::new(IndexStats::default()),
            max_k: MAX_K,
            is_mmap: false,
            current_size,
            current_ef_search,
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

        let mappings_path = path.with_extension("usearch.mappings");
        let (id_mapping, reverse_mapping, max_key, metadata) =
            load_mappings_with_integrity(&mappings_path)?;

        let (quantization, metric) = if let Some(meta) = metadata {
            if meta.dimensions != dimensions {
                return Err(Error::Vector(VectorError::IndexError(format!(
                    "Index dimension mismatch: usearch reported {}, metadata says {}",
                    dimensions, meta.dimensions
                ))));
            }
            (meta.quantization, meta.metric)
        } else {
            (Quantization::default(), DistanceMetric::Cosine)
        };

        // Initialize atomic trackers
        let current_size = AtomicUsize::new(index.size());
        let current_ef_search = AtomicUsize::new(index.expansion_search());

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
            current_size,
            current_ef_search,
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
// 8. `current_size`, `current_ef_search`: AtomicUsize is Send+Sync.
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

// ... existing tests ...
#[cfg(test)]
mod sentry_tests {
    use super::*;
    // ... same tests as before ...
    #[test]
    fn test_metric_wrapper_safe_on_unaligned() {
        let distance_fn = Arc::new(|_: &[f32], _: &[f32]| 0.0);
        let wrapper = create_metric_wrapper(4, distance_fn);
        let buffer = [0u8; 32];
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
        let config = HnswConfig {
            dimensions: 128,
            metric: DistanceMetric::Cosine,
            m: 16,
            ef_construction: 128,
            ef_search: 64,
            capacity: 1000,
            quantization: Quantization::F32,
            storage: StorageMode::InMemory,
            custom_metric: None,
        };
        let mut buffer = Vec::new();
        buffer.extend_from_slice(&(config.dimensions as u64).to_le_bytes());
        buffer.push(config.metric.to_u8());
        buffer.extend_from_slice(&(config.m as u64).to_le_bytes());
        buffer.extend_from_slice(&(config.ef_construction as u64).to_le_bytes());
        buffer.extend_from_slice(&(config.ef_search as u64).to_le_bytes());
        buffer.extend_from_slice(&(config.capacity as u64).to_le_bytes());
        let mut cursor = std::io::Cursor::new(buffer);
        let deserialized = HnswConfig::deserialize_from(&mut cursor).unwrap();
        assert_eq!(config, deserialized);
        assert_eq!(deserialized.quantization, Quantization::F32);
    }

    #[test]
    fn test_hnsw_config_deserialize_invalid_metric() {
        let mut buffer = Vec::new();
        buffer.extend_from_slice(&128u64.to_le_bytes());
        buffer.push(99);
        buffer.resize(100, 0);
        let mut cursor = std::io::Cursor::new(buffer);
        let result = HnswConfig::deserialize_from(&mut cursor);
        assert!(result.is_err());
    }

    #[test]
    fn test_hnsw_config_deserialize_invalid_quantization() {
        let config = HnswConfig::default();
        let mut buffer = Vec::new();
        buffer.extend_from_slice(&(config.dimensions as u64).to_le_bytes());
        buffer.push(config.metric.to_u8());
        buffer.extend_from_slice(&(config.m as u64).to_le_bytes());
        buffer.extend_from_slice(&(config.ef_construction as u64).to_le_bytes());
        buffer.extend_from_slice(&(config.ef_search as u64).to_le_bytes());
        buffer.extend_from_slice(&(config.capacity as u64).to_le_bytes());
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
        let res = HnswIndexBuilder::new(10, DistanceMetric::Cosine)
            .m(100)
            .build();
        assert!(res.is_err());
        let res = HnswIndexBuilder::new(10, DistanceMetric::Cosine)
            .m(0)
            .build();
        assert!(res.is_err());
        let res = HnswIndexBuilder::new(0, DistanceMetric::Cosine).build();
        assert!(res.is_err());
    }

    #[test]
    fn test_custom_metric_safety_check() {
        let result = HnswIndexBuilder::new(128, DistanceMetric::Cosine)
            .quantization(Quantization::I8)
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
    fn test_metric_wrapper_safe_on_unaligned() {
        let distance_fn = Arc::new(|_: &[f32], _: &[f32]| 0.0);
        let wrapper = create_metric_wrapper(4, distance_fn);
        let mut buffer = vec![0u8; 16 + 8];
        let aligned_ptr = buffer.as_mut_ptr();
        let unaligned_ptr = unsafe { aligned_ptr.add(1) } as *const f32;
        let valid_ptr = aligned_ptr as *const f32;
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
        let results =
            index.search_with_filter(&[1.0, 0.0, 0.0, 0.0], 3, |id| id.as_u64() % 2 == 0)?;
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
        let result = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .ef_construction(5)
            .build();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("ef_construction"));
        let result = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .ef_construction(5000)
            .build();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("ef_construction"));
        let result = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .ef_search(0)
            .build();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("ef_search"));
        let result = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .ef_search(5000)
            .build();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("ef_search"));
    }

    #[test]
    fn test_distance_to_similarity_conversion() -> Result<()> {
        let cosine_index = HnswIndexBuilder::new(3, DistanceMetric::Cosine).build()?;
        let n1 = NodeId::new(1).unwrap();
        let n2 = NodeId::new(2).unwrap();
        let n3 = NodeId::new(3).unwrap();
        cosine_index.add(n1, &[1.0, 0.0, 0.0])?;
        cosine_index.add(n2, &[0.9, 0.1, 0.0])?;
        cosine_index.add(n3, &[0.0, 1.0, 0.0])?;
        let results = cosine_index.search(&[1.0, 0.0, 0.0], 3)?;
        assert_eq!(results[0].0, n1);
        assert!(results[0].1 > 0.99);
        assert_eq!(results[1].0, n2);
        assert!(results[1].1 > 0.9);
        assert_eq!(results[2].0, n3);
        assert!(results[2].1 < 0.1 && results[2].1 > -0.1);
        Ok(())
    }

    #[test]
    fn test_update_existing_node() -> Result<()> {
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;
        let node1 = NodeId::new(1).unwrap();
        index.add(node1, &[1.0, 0.0, 0.0, 0.0])?;
        assert_eq!(index.len(), 1);
        index.add(node1, &[0.0, 1.0, 0.0, 0.0])?;
        assert_eq!(index.len(), 1);
        let results = index.search(&[0.0, 1.0, 0.0, 0.0], 1)?;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, node1);
        assert!(results[0].1 > 0.99);
        Ok(())
    }

    #[test]
    fn test_capacity_expansion_on_add() -> Result<()> {
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .initial_capacity(2)
            .build()?;
        let node1 = NodeId::new(1).unwrap();
        let node2 = NodeId::new(2).unwrap();
        index.add(node1, &[1.0, 0.0, 0.0, 0.0])?;
        index.add(node2, &[0.0, 1.0, 0.0, 0.0])?;
        assert_eq!(index.len(), 2);
        let node3 = NodeId::new(3).unwrap();
        index.add(node3, &[0.0, 0.0, 1.0, 0.0])?;
        assert_eq!(index.len(), 3);
        let node4 = NodeId::new(4).unwrap();
        let node5 = NodeId::new(5).unwrap();
        index.add(node4, &[0.0, 0.0, 0.0, 1.0])?;
        index.add(node5, &[0.5, 0.5, 0.0, 0.0])?;
        assert_eq!(index.len(), 5);
        let results = index.search(&[1.0, 0.0, 0.0, 0.0], 5)?;
        assert_eq!(results.len(), 5);
        Ok(())
    }

    #[test]
    fn test_capacity_expansion_on_update() -> Result<()> {
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .initial_capacity(2)
            .build()?;
        let node1 = NodeId::new(1).unwrap();
        let node2 = NodeId::new(2).unwrap();
        index.add(node1, &[1.0, 0.0, 0.0, 0.0])?;
        index.add(node2, &[0.0, 1.0, 0.0, 0.0])?;
        assert_eq!(index.len(), 2);
        index.add(node1, &[0.5, 0.5, 0.0, 0.0])?;
        assert_eq!(index.len(), 2);
        let node3 = NodeId::new(3).unwrap();
        index.add(node3, &[0.0, 0.0, 1.0, 0.0])?;
        assert_eq!(index.len(), 3);
        let node4 = NodeId::new(4).unwrap();
        index.add(node4, &[0.0, 0.0, 0.0, 1.0])?;
        assert_eq!(index.len(), 4);
        index.add(node2, &[0.2, 0.8, 0.0, 0.0])?;
        assert_eq!(index.len(), 4);
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
        let index = Arc::new(HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?);
        let node1 = NodeId::new(1).unwrap();
        index.add(node1, &[1.0, 0.0, 0.0, 0.0])?;
        let num_threads = 10;
        let updates_per_thread = 10;
        let mut handles = vec![];
        for thread_id in 0..num_threads {
            let index_clone = Arc::clone(&index);
            let handle = thread::spawn(move || {
                for i in 0..updates_per_thread {
                    let val = (thread_id * updates_per_thread + i) as f32 / 100.0;
                    let vector = vec![val, 1.0 - val, 0.0, 0.0];
                    index_clone.add(node1, &vector).unwrap();
                }
            });
            handles.push(handle);
        }
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(index.len(), 1);
        let results = index.search(&[0.5, 0.5, 0.0, 0.0], 1)?;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, node1);
        Ok(())
    }

    #[test]
    fn test_concurrent_mixed_operations() -> Result<()> {
        use std::sync::Arc;
        use std::thread;
        let index = Arc::new(HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?);
        let num_threads = 8;
        let mut handles = vec![];
        for thread_id in 0..num_threads {
            let index_clone = Arc::clone(&index);
            let handle = thread::spawn(move || {
                let node = NodeId::new(thread_id as u64 + 1).unwrap();
                let vector = vec![thread_id as f32 / num_threads as f32, 0.0, 0.0, 0.0];
                index_clone.add(node, &vector).unwrap();
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
        assert_eq!(index.len(), num_threads);
        let results = index.search(&[0.5, 0.5, 0.0, 0.0], num_threads)?;
        assert_eq!(results.len(), num_threads);
        Ok(())
    }

    #[test]
    fn test_max_key_overflow_protection() -> Result<()> {
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;
        const MAX_VALID_KEY: u64 = u64::MAX - 1000;
        index
            .next_key
            .store(MAX_VALID_KEY, std::sync::atomic::Ordering::SeqCst);
        let node1 = NodeId::new(1).unwrap();
        assert!(index.add(node1, &[1.0, 0.0, 0.0, 0.0]).is_ok());
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
        assert!(index.add(node1, &[0.5, 0.5, 0.0, 0.0]).is_ok());
        Ok(())
    }

    #[test]
    fn test_update_nonexistent_then_exists() -> Result<()> {
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;
        let node1 = NodeId::new(1).unwrap();
        index.add(node1, &[1.0, 0.0, 0.0, 0.0])?;
        assert_eq!(index.len(), 1);
        index.add(node1, &[0.0, 1.0, 0.0, 0.0])?;
        assert_eq!(index.len(), 1);
        let results = index.search(&[0.0, 1.0, 0.0, 0.0], 1)?;
        assert_eq!(results[0].0, node1);
        assert!(results[0].1 > 0.99);
        Ok(())
    }

    #[test]
    fn test_stats_tracking() -> Result<()> {
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;
        let node1 = NodeId::new(1).unwrap();
        let node2 = NodeId::new(2).unwrap();
        let initial_adds = index
            .stats
            .vectors_added
            .load(std::sync::atomic::Ordering::Relaxed);
        index.add(node1, &[1.0, 0.0, 0.0, 0.0])?;
        index.add(node2, &[0.0, 1.0, 0.0, 0.0])?;
        let after_adds = index
            .stats
            .vectors_added
            .load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(after_adds - initial_adds, 2);
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
        let distance_fn = Arc::new(|_: &[f32], _: &[f32]| 0.0);
        let wrapper = create_metric_wrapper(4, distance_fn);
        let vec = [0.0f32; 4];
        let valid_ptr = vec.as_ptr();
        let null_ptr = std::ptr::null();
        let result = wrapper(valid_ptr, null_ptr);
        assert_eq!(result, f32::MAX);
    }

    #[test]
    fn test_load_mappings_bad_magic() -> Result<()> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_index.usearch");
        let mappings_path = path.with_extension("usearch.mappings");
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;
        index.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0])?;
        index.save(&path)?;
        let mut data = std::fs::read(&mappings_path).unwrap();
        data[0] = b'X';
        data[1] = b'X';
        data[2] = b'X';
        data[3] = b'X';
        std::fs::write(&mappings_path, &data).unwrap();
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
        let mut data = std::fs::read(&mappings_path).unwrap();
        data[4] = 99;
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
        let mut data = std::fs::read(&mappings_path).unwrap();
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
        let data = std::fs::read(&mappings_path).unwrap();
        let truncated = &data[..10];
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
        let mut data = std::fs::read(&mappings_path).unwrap();
        let count_offset = 15;
        data[count_offset] = 2;
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
        let mut data = std::fs::read(&mappings_path).unwrap();
        let count_offset = 15;
        let huge_count = u64::MAX;
        let count_bytes = huge_count.to_le_bytes();
        data[count_offset..count_offset + 8].copy_from_slice(&count_bytes);
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
        let mut data = std::fs::read(&mappings_path).unwrap();
        let count_offset = 15;
        let huge_count = (super::MAX_MAPPINGS_COUNT + 1) as u64;
        let count_bytes = huge_count.to_le_bytes();
        data[count_offset..count_offset + 8].copy_from_slice(&count_bytes);
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
        let count = 2000;
        for i in 1..=count {
            index.add(NodeId::new(i).unwrap(), &[1.0, 0.0, 0.0, 0.0])?;
        }
        index.save(&path)?;
        let loaded = HnswIndex::load(&path, HnswConfig::new(4, DistanceMetric::Cosine))?;
        assert_eq!(loaded.len(), count as usize);
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
            Ok(())
        }
    }

    #[test]
    fn test_save_mappings_write_errors() {
        let mappings = [
            (NodeId::new(1).unwrap(), 100),
            (NodeId::new(2).unwrap(), 200),
        ];
        let config = HnswConfig::default();
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
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap();
        let index_path = dir.path().join("test.index");
        let mappings_path = index_path.with_extension("usearch.mappings");
        std::fs::create_dir(&mappings_path).unwrap();
        let result = index.save(&index_path);
        assert!(result.is_err());
        if let Err(Error::Vector(VectorError::IndexError(msg))) = result {
            assert!(msg.contains("Failed to create mappings file"));
        } else {
            panic!("Expected IndexError, got {:?}", result);
        }
    }

    #[test]
    fn test_custom_metric_execution_coverage() {
        let metric_fn = |a: &[f32], b: &[f32]| -> f32 {
            a.iter().zip(b.iter()).map(|(x, y)| (x - y).abs()).sum()
        };
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .quantization(Quantization::F32)
            .with_custom_metric("manhattan", metric_fn)
            .build()
            .unwrap();
        for i in 0..10 {
            let id = NodeId::new(i + 1).unwrap();
            let vec = if i % 2 == 0 {
                [1.0, 0.0, 0.0, 0.0]
            } else {
                [0.0, 1.0, 0.0, 0.0]
            };
            index.add(id, &vec).unwrap();
        }
        let results = index.search(&[0.9, 0.1, 0.0, 0.0], 5).unwrap();
        assert_eq!(results.len(), 5);
    }

    #[test]
    fn test_add_race_retry_value_change_coverage() {
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap();
        let node = NodeId::new(1).unwrap();
        index.add(node, &[1.0, 0.0, 0.0, 0.0]).unwrap();
        TEST_RACE_HOOK.with(|h| {
            h.set(Some(|idx, node_id| {
                idx.id_mapping.insert(node_id, 999);
                idx.reverse_mapping.insert(999, node_id);
            }))
        });
        let result = index.add(node, &[0.0, 1.0, 0.0, 0.0]);
        TEST_RACE_HOOK.with(|h| h.set(None));
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
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap();
        let node = NodeId::new(2).unwrap();
        index.add(node, &[1.0, 0.0, 0.0, 0.0]).unwrap();
        TEST_RACE_HOOK.with(|h| {
            h.set(Some(|idx, node_id| {
                idx.id_mapping.remove(&node_id);
            }))
        });
        let result = index.add(node, &[0.0, 1.0, 0.0, 0.0]);
        TEST_RACE_HOOK.with(|h| h.set(None));
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
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap();
        let node = NodeId::new(3).unwrap();
        TEST_RACE_HOOK.with(|h| {
            h.set(Some(|idx, node_id| {
                idx.id_mapping.insert(node_id, 999);
            }))
        });
        let result = index.add(node, &[0.5, 0.5, 0.5, 0.5]);
        TEST_RACE_HOOK.with(|h| h.set(None));
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::IndexError(msg))) => {
                assert!(msg.contains("Concurrent add detected"));
                assert!(msg.contains("vector already exists"));
            }
            _ => panic!("Expected concurrent add error, got {:?}", result),
        }
        assert_eq!(index.len(), 0);
    }

    #[test]
    fn test_save_coverage() -> Result<()> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("coverage.index");
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;
        index.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0])?;
        index.save(&path)?;
        assert!(path.exists());
        Ok(())
    }
}

// ... warden_tests and coverage_tests ...
#[cfg(test)]
mod warden_tests {
    use super::*;
    use crate::core::property::MAX_VECTOR_DIMENSIONS;

    #[test]
    fn test_config_deserialize_dimensions_too_large() {
        let huge_dims = (MAX_VECTOR_DIMENSIONS + 1) as u64;
        let mut buffer = Vec::new();
        buffer.extend_from_slice(&huge_dims.to_le_bytes());
        buffer.push(0);
        buffer.extend_from_slice(&16u64.to_le_bytes());
        buffer.extend_from_slice(&128u64.to_le_bytes());
        buffer.extend_from_slice(&64u64.to_le_bytes());
        buffer.extend_from_slice(&1000u64.to_le_bytes());
        buffer.push(0);

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
        let config = HnswConfig {
            dimensions: MAX_VECTOR_DIMENSIONS + 1,
            ..Default::default()
        };
        let result = HnswIndex::load(&path, config);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("dimensions"));
        assert!(msg.contains("exceeds maximum allowed"));
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
        let result = wrapper(unaligned_ptr, valid_ptr);
        assert_eq!(result, f32::MAX);
    }

    #[test]
    fn test_filter_callback_guard_reset() {
        IN_FILTER_CALLBACK.with(|flag| flag.set(false));
        {
            let _guard = FilterCallbackGuard::new();
            assert!(IN_FILTER_CALLBACK.with(|flag| flag.get()));
        }
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
        let distance_fn = Arc::new(|_: &[f32], _: &[f32]| -> f32 {
            panic!("Test panic");
        });
        let wrapper = create_metric_wrapper(4, distance_fn);
        let data = [0.0f32; 4];
        let ptr = data.as_ptr();
        let result = wrapper(ptr, ptr);
        assert_eq!(result, f32::MAX);
    }

    #[test]
    fn test_metric_wrapper_success_direct() {
        let distance_fn = Arc::new(|a: &[f32], b: &[f32]| -> f32 {
            a.iter().zip(b.iter()).map(|(x, y)| (x - y).abs()).sum()
        });
        let wrapper = create_metric_wrapper(4, distance_fn);
        let data_a = [1.0f32, 2.0, 3.0, 4.0];
        let data_b = [1.5f32, 2.5, 3.5, 4.5];
        let result = wrapper(data_a.as_ptr(), data_b.as_ptr());
        assert!((result - 2.0).abs() < f32::EPSILON);
    }
}
