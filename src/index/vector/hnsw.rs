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

// Thread-local flag to detect re-entrant modification attempts.
// This prevents deadlocks when user filter callbacks or custom metrics try to modify the index
// while holding the inner lock (read).
std::thread_local! {
    pub(crate) static IN_FILTER_CALLBACK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static REENTRANCY_GUARD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// RAII guard that sets REENTRANCY_GUARD to true on creation and false on drop.
/// This ensures the flag is always reset, even if the callback panics.
pub(crate) struct FilterCallbackGuard;

impl FilterCallbackGuard {
    pub(crate) fn new() -> Self {
        IN_FILTER_CALLBACK.with(|flag| flag.set(true));
        FilterCallbackGuard
    }
}

impl Drop for FilterCallbackGuard {
    fn drop(&mut self) {
        IN_FILTER_CALLBACK.with(|flag| flag.set(false));
    }
}

/// It also handles nested usage correctly by restoring the previous state.
struct ReentrancyGuard {
    prev: bool,
}

impl ReentrancyGuard {
    fn new() -> Self {
        let prev = REENTRANCY_GUARD.with(|flag| flag.replace(true));
        ReentrancyGuard { prev }
    }
}

impl Drop for ReentrancyGuard {
    fn drop(&mut self) {
        REENTRANCY_GUARD.with(|flag| flag.set(self.prev));
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

// Helper to abort safely on FFI violations
// This is excluded from coverage because it terminates the process and cannot be tested
#[cold]
#[inline(never)]
fn ffi_abort(reason: &str) -> ! {
    #[cfg(test)]
    {
        panic!(
            "CRITICAL SECURITY ERROR: {}. Aborting to prevent UB.",
            reason
        );
    }

    #[cfg(not(test))]
    {
        use std::io::Write;
        let _ = writeln!(
            std::io::stderr(),
            "CRITICAL SECURITY ERROR: {}. Aborting to prevent UB.",
            reason
        );
        std::process::abort();
    }
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
        // Set re-entrancy guard to prevent deadlock if custom metric calls back into index
        // This is critical because usearch holds a read lock while calling this metric.
        let _guard = ReentrancyGuard::new();

        // Check for null pointers to prevent UB
        if a.is_null() || b.is_null() {
            // This should never happen with a correct usearch implementation.
            // If it does, we MUST abort to prevent UB from dereferencing null or
            // unwinding across the FFI boundary (which is UB).
            // We cannot return an error here because the signature is fixed by usearch trait.
            ffi_abort("usearch passed null pointer to metric function");
        }

        // Check for alignment to prevent UB
        // Use bitwise check for power-of-2 alignment (f32 align is 4)
        let align_mask = std::mem::align_of::<f32>() - 1;
        if (a as usize) & align_mask != 0 || (b as usize) & align_mask != 0 {
            // Abort for same reason as above: unwinding across FFI is UB.
            ffi_abort("usearch passed unaligned pointer to metric function");
        }

        // SAFETY: usearch guarantees pointers are valid for `dims` elements.
        // We verified they are not null above.

        let slice_a = unsafe { std::slice::from_raw_parts(a, dims) };
        let slice_b = unsafe { std::slice::from_raw_parts(b, dims) };

        // Prevent re-entrant modifications during metric calculation (deadlock prevention)
        // This sets the thread-local flag so that add() and other methods fail gracefully.
        // Without this, a custom metric calling add() would deadlock on the inner RwLock.
        let _guard = FilterCallbackGuard::new();

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MappingAddOutcome {
    UpdateExisting,
    InsertNew,
    RollbackAfterRace,
}

#[inline]
fn classify_mapping_add_outcome(existing_mapping: bool, race_detected: bool) -> MappingAddOutcome {
    if existing_mapping {
        MappingAddOutcome::UpdateExisting
    } else if race_detected {
        MappingAddOutcome::RollbackAfterRace
    } else {
        MappingAddOutcome::InsertNew
    }
}

impl VectorIndex for HnswIndex {
    fn add(&self, id: NodeId, vector: &[f32]) -> Result<()> {
        // Check for re-entrant modification during callbacks (prevents deadlock)
        if REENTRANCY_GUARD.with(|flag| flag.get()) {
            return Err(Error::Vector(VectorError::IndexError(
                "Cannot modify index from within a callback (filter or metric). \
                 This would cause a deadlock due to lock re-entrancy. \
                 Consider collecting modifications and applying them after the operation completes."
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
                debug_assert_eq!(
                    classify_mapping_add_outcome(true, false),
                    MappingAddOutcome::UpdateExisting
                );
                // Re-adding existing node: remove old vector from usearch if it exists
                // Optimization (Issue #207): Only call remove() if key actually exists in usearch.
                // This avoids unnecessary FFI calls during recovery or when mappings are out of sync.
                let existing_key = *entry.get();

                // CRITICAL: Hold write lock continuously from remove to add to prevent race conditions
                // where multiple threads try to update the same node concurrently (PR #575).
                // Without this, thread A could remove, thread B could remove (fail), then both try to add,
                // causing "Duplicate keys not allowed" error.
                let index = self.inner.write();

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

                // Release inner lock before accessing DashMap
                drop(index);

                // Step 4: Insert to mappings (dashmap) AFTER inner is updated
                // Handle race: another thread may have added this NodeId while we held inner lock
                // Use entry API to safely check for existence without overwriting (which causes Zombie Vectors)
                let race_detected = match self.id_mapping.entry(id) {
                    dashmap::mapref::entry::Entry::Occupied(_) => true,
                    dashmap::mapref::entry::Entry::Vacant(e) => {
                        // Success: we claimed the ID
                        e.insert(key);
                        // Drop the entry lock implicitly here when e is consumed/scope ends
                        false
                    }
                };

                if classify_mapping_add_outcome(false, race_detected)
                    == MappingAddOutcome::RollbackAfterRace
                {
                    // Race detected: Another thread added this NodeId concurrently
                    // Our vector is in inner with key=key, but someone else claimed the ID.
                    // We must rollback our addition to avoid phantom vectors.

                    // Acquire inner lock again to remove our key.
                    // We do this AFTER releasing the id_mapping lock to minimize contention and deadlock risk.
                    let index = self.inner.write();
                    index.remove(key).map_err(|e| {
                        Error::Vector(VectorError::IndexError(format!(
                            "Failed to rollback vector after concurrent add: {}",
                            e
                        )))
                    })?;

                    // The existing mapping wins; return error to indicate retry needed
                    return Err(Error::Vector(VectorError::IndexError(
                        "Concurrent add detected for same NodeId, vector already exists"
                            .to_string(),
                    )));
                }

                // If no race, we successfully inserted into id_mapping.
                // Now insert reverse mapping.
                // Note: We do this outside the id_mapping lock to reduce contention.
                self.reverse_mapping.insert(key, id);
                self.stats.vectors_added.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
        }
    }

    fn remove(&self, id: NodeId) -> Result<()> {
        // Check for re-entrant modification during callbacks (prevents deadlock)
        if REENTRANCY_GUARD.with(|flag| flag.get()) {
            return Err(Error::Vector(VectorError::IndexError(
                "Cannot modify index from within a callback (filter or metric). \
                 This would cause a deadlock due to lock re-entrancy. \
                 Consider collecting modifications and applying them after the operation completes."
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
        // We set REENTRANCY_GUARD flag when calling the user's predicate to detect
        // and prevent re-entrant modification attempts that would cause deadlock.
        let reverse_mapping = &self.reverse_mapping;
        let filter = |key: u64| -> bool {
            if let Some(node_id_ref) = reverse_mapping.get(&key) {
                // Set flag to prevent modifications during callback
                let _guard = ReentrancyGuard::new();
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
        // Check for re-entrant modification during callbacks (prevents deadlock)
        if REENTRANCY_GUARD.with(|flag| flag.get()) {
            return Err(Error::Vector(VectorError::IndexError(
                "Cannot save index from within a callback (filter or metric). \
                 This would cause a deadlock due to lock re-entrancy. \
                 Consider saving after the operation completes."
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
    /// Internal implementation of index saving.
    ///
    /// This method performs the actual blocking I/O operations for saving the index
    /// and its mappings. It is separated from `save()` to allow the latter to use
    /// `tokio::task::block_in_place` when running within a Tokio runtime.
    fn save_internal(&self, path: &Path) -> Result<()> {
        // DEADLOCK FIX (PR #751): Collect mappings BEFORE acquiring any locks
        // This prevents lock ordering deadlock with add() which holds DashMap → inner lock order
        //
        // Lock Ordering Invariant:
        //   1. inner (RwLock<Index>) - FIRST
        //   2. id_mapping (DashMap) - SECOND
        //
        // Previous implementation violated this by:
        //   1. Acquiring inner.read() first (line 991)
        //   2. Then iterating id_mapping (line 1032), acquiring DashMap shard locks
        //
        // Meanwhile, add() (Occupied path) acquires locks in reverse order:
        //   1. DashMap shard lock via entry() (line 634)
        //   2. Then inner.write() (line 645)
        //
        // Result: Classic lock inversion deadlock.
        //
        // Solution: Collect all mappings into Vec with no locks held, sacrificing
        // the "⚡ Bolt Optimization" streaming approach for correctness.
        // Memory cost: O(N) allocation (~16MB for 1M nodes), acceptable for infrequent save operation.
        let mappings: Vec<(NodeId, u64)> = self
            .id_mapping
            .iter()
            .map(|e| (*e.key(), *e.value()))
            .collect();
        let count = mappings.len();

        // Now acquire inner.read() with no other locks held
        let index = self.inner.read();
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
                    // usearch IP metric returns (1 - dot_product)
                    // We want to return dot_product, so: 1.0 - (1.0 - dot) = dot
                    DistanceMetric::DotProduct => 1.0 - distance,
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
        let (id_mapping, reverse_mapping, max_key, metadata) =
            load_mappings_with_integrity(&mappings_path)?;

        // Validate metadata
        Self::validate_metadata(metadata, &config)?;

        // Verify loaded index dimensions match configuration
        // This protects against legacy indexes (no metadata) having mismatched dimensions,
        // which could lead to buffer over-reads if usearch didn't have its own checks.
        let loaded_dims = index.dimensions();
        if loaded_dims != config.dimensions {
            return Err(Error::Vector(VectorError::IndexError(format!(
                "Index dimension mismatch: config={}, actual loaded={}",
                config.dimensions, loaded_dims
            ))));
        }

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

    // TEST REMOVED: test_metric_wrapper_panic_on_unaligned
    // Reason: The metric wrapper now calls std::process::abort() instead of panic!
    // for security reasons (preventing FFI unwind UB).
    // Abort terminates the test runner and cannot be caught by #[should_panic].

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
    fn test_mcdc_classify_mapping_add_outcome() {
        // A=false, B=false
        assert_eq!(
            classify_mapping_add_outcome(false, false),
            MappingAddOutcome::InsertNew
        );
        // A=true, B=false
        assert_eq!(
            classify_mapping_add_outcome(true, false),
            MappingAddOutcome::UpdateExisting
        );
        // A=false, B=true
        assert_eq!(
            classify_mapping_add_outcome(false, true),
            MappingAddOutcome::RollbackAfterRace
        );
        // A=true, B=true (existing mapping dominates decision)
        assert_eq!(
            classify_mapping_add_outcome(true, true),
            MappingAddOutcome::UpdateExisting
        );
    }

    #[test]
    fn test_hnsw_basic() -> Result<()> {
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;

        let node1 = NodeId::new(1).unwrap();
        let node2 = NodeId::new(2).unwrap();

        index.add(node1, &[1.0, 0.0, 0.0, 0.0])?;
        index.add(node2, &[0.0, 1.0, 0.0, 0.0])?;

        assert_eq!(index.len(), 2);

        let results = index.search(&[1.0, 0.0, 0.0, 0.0], 2)?;
        assert_eq!(results.len(), 2);

        // Verify first result (identical match)
        assert_eq!(results[0].0, node1);
        assert!(
            (results[0].1 - 1.0).abs() < 1e-5,
            "Expected similarity ~1.0, got {}",
            results[0].1
        );

        // Verify second result (orthogonal)
        assert_eq!(results[1].0, node2);
        assert!(
            (results[1].1 - 0.0).abs() < 1e-5,
            "Expected similarity ~0.0, got {}",
            results[1].1
        );

        Ok(())
    }

    // TEST REMOVED: test_metric_wrapper_panic_on_unaligned
    // Reason: The metric wrapper now calls std::process::abort() instead of panic!
    // for security reasons (preventing FFI unwind UB).
    // Abort terminates the test runner and cannot be caught by #[should_panic].

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
        let epsilon = 1e-4;

        // 1. Cosine: similarity = 1.0 - distance
        // usearch Cosine distance is 1.0 - cosine_similarity (range [0, 2])
        // So our similarity should be cosine_similarity (range [-1, 1])
        {
            let index = HnswIndexBuilder::new(2, DistanceMetric::Cosine).build()?;
            let n1 = NodeId::new(1).unwrap();
            let n2 = NodeId::new(2).unwrap();
            let n3 = NodeId::new(3).unwrap();

            // v1 = [1, 0]
            // v2 = [0, 1] (orthogonal, sim 0.0, dist 1.0)
            // v3 = [-1, 0] (opposite, sim -1.0, dist 2.0)
            index.add(n1, &[1.0, 0.0])?;
            index.add(n2, &[0.0, 1.0])?;
            index.add(n3, &[-1.0, 0.0])?;

            let results = index.search(&[1.0, 0.0], 3)?;

            // Expected: n1 (1.0), n2 (0.0), n3 (-1.0)
            assert_eq!(results[0].0, n1);
            assert!(
                (results[0].1 - 1.0).abs() < epsilon,
                "Cosine n1 should be 1.0, got {}",
                results[0].1
            );

            assert_eq!(results[1].0, n2);
            assert!(
                (results[1].1 - 0.0).abs() < epsilon,
                "Cosine n2 should be 0.0, got {}",
                results[1].1
            );

            assert_eq!(results[2].0, n3);
            assert!(
                (results[2].1 - -1.0).abs() < epsilon,
                "Cosine n3 should be -1.0, got {}",
                results[2].1
            );
        }

        // 2. Euclidean: similarity = -distance
        // usearch Euclidean is L2 squared.
        {
            let index = HnswIndexBuilder::new(2, DistanceMetric::Euclidean).build()?;
            let n1 = NodeId::new(1).unwrap();
            let n2 = NodeId::new(2).unwrap();

            // v1 = [0, 0]
            // v2 = [3, 4] (distance = sqrt(3^2 + 4^2) = 5. Squared = 25)
            index.add(n1, &[0.0, 0.0])?;
            index.add(n2, &[3.0, 4.0])?;

            let results = index.search(&[0.0, 0.0], 2)?;

            // Expected: n1 (sim = -0 = 0), n2 (sim = -25)
            assert_eq!(results[0].0, n1);
            assert!(
                (results[0].1 - 0.0).abs() < epsilon,
                "Euclidean n1 should be 0.0, got {}",
                results[0].1
            );

            assert_eq!(results[1].0, n2);
            assert!(
                (results[1].1 - -25.0).abs() < epsilon,
                "Euclidean n2 should be -25.0, got {}",
                results[1].1
            );
        }

        // 3. DotProduct: similarity = 1.0 - distance
        // usearch IP distance is 1.0 - dot_product
        // So similarity = 1.0 - (1.0 - dot_product) = dot_product
        {
            let index = HnswIndexBuilder::new(2, DistanceMetric::DotProduct).build()?;
            let n1 = NodeId::new(1).unwrap();
            let n2 = NodeId::new(2).unwrap();

            // v1 = [1, 2]
            // v2 = [3, 4]
            // query = [1, 2]
            // n1 dot query = 1*1 + 2*2 = 5.
            // n2 dot query = 3*1 + 4*2 = 3 + 8 = 11.
            // n2 should be first!
            index.add(n1, &[1.0, 2.0])?;
            index.add(n2, &[3.0, 4.0])?;

            let results = index.search(&[1.0, 2.0], 2)?;

            // Expected: n2 (dot 11), n1 (dot 5)
            assert_eq!(results[0].0, n2);
            assert!(
                (results[0].1 - 11.0).abs() < epsilon,
                "DotProduct n2 should be 11.0, got {}",
                results[0].1
            );

            assert_eq!(results[1].0, n1);
            assert!(
                (results[1].1 - 5.0).abs() < epsilon,
                "DotProduct n1 should be 5.0, got {}",
                results[1].1
            );
        }

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

    // TEST REMOVED: test_metric_wrapper_panic_on_null
    // Reason: The metric wrapper now calls std::process::abort() instead of panic!
    // for security reasons (preventing FFI unwind UB).
    // Abort terminates the test runner and cannot be caught by #[should_panic].

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
    fn test_filter_callback_guard_drop() {
        // Verify that FilterCallbackGuard correctly resets the thread-local flag on Drop

        // Initial state should be false
        assert!(!super::IN_FILTER_CALLBACK.with(|flag| flag.get()));

        {
            let _guard = super::FilterCallbackGuard::new();
            // Inside scope, flag should be true
            assert!(super::IN_FILTER_CALLBACK.with(|flag| flag.get()));
        }

        // After drop, flag should be false
        assert!(!super::IN_FILTER_CALLBACK.with(|flag| flag.get()));
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;

    #[test]
    #[should_panic(expected = "usearch passed null pointer")]
    fn test_metric_wrapper_null_pointer() {
        let distance_fn = Arc::new(|_: &[f32], _: &[f32]| 0.0);
        let wrapper = create_metric_wrapper(4, distance_fn);

        let null_ptr: *const f32 = std::ptr::null();
        let valid_data = [0.0f32; 4];
        let valid_ptr = valid_data.as_ptr();

        // This should panic
        wrapper(null_ptr, valid_ptr);
    }

    #[test]
    #[should_panic(expected = "usearch passed unaligned pointer")]
    fn test_metric_wrapper_unaligned_pointer() {
        let distance_fn = Arc::new(|_: &[f32], _: &[f32]| 0.0);
        let wrapper = create_metric_wrapper(4, distance_fn);

        let data = [0u8; 32];
        let unaligned_ptr = unsafe { data.as_ptr().add(1) as *const f32 };
        let valid_data = [0.0f32; 4];
        let valid_ptr = valid_data.as_ptr();

        // This should panic
        wrapper(unaligned_ptr, valid_ptr);
    }
}
