//! HNSW (Hierarchical Navigable Small World) vector index implementation.
//!
//! This module provides a wrapper around the `usearch` library's HNSW index,
//! implementing the `VectorIndex` trait for approximate k-nearest neighbor search.

use crate::core::error::{Error, Result, VectorError};
use crate::core::id::NodeId;
use crate::core::property::MAX_VECTOR_DIMENSIONS;
use crate::core::vector::validate_vector;
use crate::index::vector::{DistanceMetric, Quantization, StorageMode, VectorIndex};
use dashmap::DashMap;
use parking_lot::{Mutex, RwLock};
use std::fs::File;
use std::io::BufWriter;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use usearch::{Index, IndexOptions, MetricKind, ScalarKind, ffi::Matches};

/// HNSW configuration and builder.
pub mod config;
/// Persistence logic for HNSW index.
pub mod persistence;
/// Statistics for HNSW index.
pub mod stats;

#[cfg(test)]
mod tests;

pub use config::{HnswConfig, HnswIndexBuilder};
use persistence::{load_mappings_with_integrity, verify_index_header, write_mappings_to_writer};
use stats::{IndexStats, MAX_SEARCH_ATTEMPTS};

// Thread-local flag to detect re-entrant modification attempts during filtered search.
std::thread_local! {
    pub(crate) static IN_FILTER_CALLBACK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
type TestRaceHook = fn(&HnswIndex, NodeId);

#[cfg(test)]
std::thread_local! {
    pub(crate) static TEST_RACE_HOOK: std::cell::Cell<Option<TestRaceHook>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(crate) static TEST_SKIP_CAPACITY_CHECK: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// RAII guard that sets IN_FILTER_CALLBACK to true on creation and restores previous value on drop.
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

/// Maximum number of results that can be requested in a search.
const MAX_K: usize = 100_000;

/// Number of sharded locks for entry updates.
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

/// Check if a usearch error is transient and should be retried.
pub(crate) fn is_retryable_usearch_error(error_msg: &str) -> bool {
    error_msg.contains("No available threads to lock")
}

// Helper to create the metric wrapper
pub(crate) fn create_metric_wrapper<F>(
    dims: usize,
    distance_fn: Arc<F>,
) -> Box<dyn Fn(*const f32, *const f32) -> f32 + Send + Sync>
where
    F: Fn(&[f32], &[f32]) -> f32 + Send + Sync + 'static + ?Sized,
{
    Box::new(move |a: *const f32, b: *const f32| {
        if a.is_null() || b.is_null() {
            eprintln!("usearch passed null pointer to metric function - returning max distance");
            return f32::MAX;
        }

        let align_mask = std::mem::align_of::<f32>() - 1;
        if (a as usize) & align_mask != 0 || (b as usize) & align_mask != 0 {
            eprintln!(
                "usearch passed unaligned pointer to metric function (expected alignment {}) - returning max distance",
                std::mem::align_of::<f32>()
            );
            return f32::MAX;
        }

        let slice_a = unsafe { std::slice::from_raw_parts(a, dims) };
        let slice_b = unsafe { std::slice::from_raw_parts(b, dims) };

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            distance_fn(slice_a, slice_b)
        }));

        match result {
            Ok(val) => val,
            Err(_) => {
                eprintln!(
                    "Panic in custom metric function - returning max distance to avoid FFI UB"
                );
                f32::MAX
            }
        }
    })
}

/// HNSW vector index for approximate k-nearest neighbor search.
///
/// This struct wraps the `usearch` library's HNSW implementation, providing a thread-safe,
/// persistent, and highly optimized vector index. It manages the mapping between
/// AletheiaDB's `NodeId` and the internal integer keys used by HNSW.
///
/// # Concurrency
///
/// The index uses a hybrid locking strategy to maximize concurrency:
/// - **Read path (`search`)**: Uses shared read locks (`RwLock::read`) on the inner index.
///   Multiple threads can search concurrently.
/// - **Write path (`add`, `remove`)**: Uses fine-grained sharded locks (`entry_locks`) to
///   serialize updates to the same node, preventing race conditions. The inner index lock
///   is upgraded to write only when necessary (e.g., resizing capacity).
/// - **Persistence (`save`)**: Acquires a global `save_lock` to ensure a consistent snapshot
///   of both the index structure and the ID mappings.
///
/// # Persistence
///
/// The index supports two persistence modes:
/// 1. **In-Memory**: Fast, ephemeral updates. Can be saved to disk via `save()`.
/// 2. **Memory-Mapped**: Zero-copy loading from disk for instant startup. Read-only.
pub struct HnswIndex {
    /// Underlying usearch index (C++ wrapper).
    /// Protected by RwLock for thread safety.
    pub(crate) inner: Arc<RwLock<Index>>,
    /// Configuration used to create this index.
    pub(crate) config: HnswConfig,
    /// ID mapping: NodeId -> usearch key (u64).
    /// DashMap allows concurrent lock-free reads and sharded writes.
    pub(crate) id_mapping: Arc<DashMap<NodeId, u64>>,
    /// Reverse mapping: usearch key -> NodeId.
    /// Used to reconstruct NodeIds from search results.
    pub(crate) reverse_mapping: Arc<DashMap<u64, NodeId>>,
    /// Next available internal key.
    /// Monotonically increasing counter for assigning unique keys to new vectors.
    pub(crate) next_key: AtomicU64,
    /// Runtime statistics (searches, adds, retries).
    pub(crate) stats: Arc<IndexStats>,
    /// Maximum k allowed in search queries (DoS protection).
    max_k: usize,
    /// Whether this index is memory-mapped (read-only).
    is_mmap: bool,
    /// Global lock to ensure consistency between index and mapping during save operations.
    /// Acquired in WRITE mode during save, READ mode during add/remove.
    save_lock: Arc<RwLock<()>>,
    /// Sharded locks to serialize updates to the same key/node.
    /// Prevents "lost update" races where two threads try to update the same vector simultaneously.
    entry_locks: Vec<Mutex<()>>,
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

impl std::fmt::Debug for HnswIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HnswIndex")
            .field("config", &self.config)
            .field("is_mmap", &self.is_mmap)
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

impl HnswIndex {
    /// Internal constructor used by `HnswIndexBuilder`.
    ///
    /// Validates configuration and initializes the underlying `usearch` index.
    ///
    /// # Arguments
    ///
    /// * `config` - Validated HNSW configuration.
    ///
    /// # Errors
    ///
    /// Returns error if:
    /// - Dimensions, M, or ef_construction are out of valid ranges.
    /// - Custom metric is used with non-F32 quantization.
    /// - Failed to initialize or reserve capacity in the underlying index.
    pub(crate) fn new_internal(config: HnswConfig) -> Result<Self> {
        // Validate dimensions
        if config.dimensions == 0 {
            return Err(Error::Vector(VectorError::InvalidVector {
                reason: "dimensions must be > 0".to_string(),
            }));
        }
        if config.dimensions > MAX_VECTOR_DIMENSIONS {
            return Err(Error::Vector(VectorError::InvalidVector {
                reason: format!(
                    "dimensions {} exceeds maximum allowed {}",
                    config.dimensions, MAX_VECTOR_DIMENSIONS
                ),
            }));
        }

        // Validate M
        if config.m == 0 || config.m > 64 {
            return Err(Error::Vector(VectorError::InvalidVector {
                reason: format!("M must be in range [1, 64], got {}", config.m),
            }));
        }

        // Validate ef_construction
        if config.ef_construction < 10 || config.ef_construction > 4096 {
            return Err(Error::Vector(VectorError::InvalidVector {
                reason: format!(
                    "ef_construction must be in range [10, 4096], got {}",
                    config.ef_construction
                ),
            }));
        }

        // Validate ef_search
        if config.ef_search < 1 || config.ef_search > 4096 {
            return Err(Error::Vector(VectorError::InvalidVector {
                reason: format!(
                    "ef_search must be in range [1, 4096], got {}",
                    config.ef_search
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
                "Failed to create usearch index: {}",
                e
            )))
        })?;

        let capacity_to_reserve = if config.capacity > 0 {
            config.capacity
        } else {
            1024
        };
        index.reserve(capacity_to_reserve).map_err(|e| {
            Error::Vector(VectorError::IndexError(format!(
                "Failed to reserve capacity: {}",
                e
            )))
        })?;

        if let Some(ref custom) = config.custom_metric {
            let dims = config.dimensions;
            let distance_fn = Arc::clone(&custom.distance_fn);
            let metric_wrapper = create_metric_wrapper(dims, distance_fn);
            index.change_metric(metric_wrapper);
        }

        if let StorageMode::MemoryMapped { ref path } = config.storage {
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
            config,
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

    /// Creates a new in-memory HNSW index from a configuration.
    ///
    /// This is a convenience wrapper around `HnswIndexBuilder`.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use aletheiadb::index::vector::{HnswIndex, HnswConfig, DistanceMetric};
    ///
    /// let config = HnswConfig::new(384, DistanceMetric::Cosine);
    /// let index = HnswIndex::new(config).unwrap();
    /// ```
    pub fn new(config: HnswConfig) -> Result<Self> {
        HnswIndexBuilder::from_config(&config).build()
    }

    /// Set the `ef_search` parameter (query-time search quality).
    ///
    /// Higher values increase recall (quality) but decrease search speed.
    /// Typical values are between `M` and `ef_construction`.
    ///
    /// # Thread Safety
    ///
    /// This operation acquires a read lock on the inner index, so it can be called
    /// concurrently with other searches, but acts as a dynamic configuration change.
    pub fn set_ef_search(&self, ef_search: usize) {
        let index = self.inner.read();
        index.change_expansion_search(ef_search);
    }

    /// Get the current `ef_search` parameter.
    pub fn get_ef_search(&self) -> usize {
        self.inner.read().expansion_search()
    }

    /// Get the configuration used to create this index.
    pub fn config(&self) -> HnswConfig {
        self.config.clone()
    }

    /// Get the `M` parameter (max connections per node).
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

    /// Load a persisted index from disk into memory.
    ///
    /// This loads the HNSW graph structure and vector data into RAM.
    /// For large indexes where RAM is limited, consider `open_mmap` instead.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the index file (e.g., `index.usearch`).
    /// * `config` - Configuration must match the persisted index (dimensions, metric).
    ///
    /// # Errors
    ///
    /// Returns error if:
    /// - File does not exist or is corrupted.
    /// - Dimensions in config do not match the file.
    /// - Associated mapping file (`.usearch.mappings`) is missing or invalid.
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

        verify_index_header(path, config.dimensions, config.quantization)?;

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

        if index.dimensions() != config.dimensions {
            return Err(Error::Vector(VectorError::IndexError(format!(
                "Index dimension mismatch: usearch index has {}, config has {}",
                index.dimensions(),
                config.dimensions
            ))));
        }

        if let Some(ref custom) = config.custom_metric {
            let dims = config.dimensions;
            let distance_fn = Arc::clone(&custom.distance_fn);
            let metric_wrapper = create_metric_wrapper(dims, distance_fn);
            index.change_metric(metric_wrapper);
        }

        let mappings_path = path.with_extension("usearch.mappings");
        let (id_mapping, reverse_mapping, max_key, metadata) =
            load_mappings_with_integrity(&mappings_path)?;

        persistence::validate_metadata(metadata, &config)?;

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

    /// Open a memory-mapped index from disk (read-only).
    ///
    /// Memory mapping allows accessing indexes larger than available RAM by relying
    /// on the OS page cache. This mode is read-only; attempts to add/remove vectors
    /// will return an error.
    ///
    /// # Performance Note
    ///
    /// Cold queries may trigger disk I/O, causing latency spikes. Warm queries
    /// are comparable to in-memory performance.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the index file.
    ///
    /// # Errors
    ///
    /// Returns error if:
    /// - OS fails to map the file.
    /// - Index metadata (header) is invalid.
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

        verify_index_header(path, dimensions, quantization)?;

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

    /// Helper to execute potentially blocking operations.
    ///
    /// If running within a Tokio multi-threaded runtime, this offloads the
    /// operation to a blocking thread to avoid starving the async reactor.
    pub(crate) fn maybe_block_in_place<F, R>(&self, f: F) -> R
    where
        F: FnOnce() -> R,
    {
        #[cfg(any(feature = "tokio", feature = "embeddings"))]
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            #[allow(clippy::collapsible_if)]
            if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread {
                return tokio::task::block_in_place(f);
            }
        }
        f()
    }

    /// Helper to retry usearch operations on transient errors.
    pub(crate) fn retry_usearch<F, T, E>(&self, mut op: F, context: &str) -> Result<T>
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

    pub(crate) fn check_and_expand_capacity(&self, vectors_to_add: usize) -> Result<()> {
        #[cfg(test)]
        if TEST_SKIP_CAPACITY_CHECK.load(Ordering::Relaxed) {
            return Ok(());
        }

        const CAPACITY_PADDING: usize = 128;

        let index = self.inner.read();
        if index.size() + vectors_to_add + CAPACITY_PADDING <= index.capacity() {
            return Ok(());
        }
        drop(index);

        let index = self.inner.write();
        if index.size() + vectors_to_add + CAPACITY_PADDING <= index.capacity() {
            return Ok(());
        }

        let new_capacity = (index.capacity() * 2).max(1024);
        self.retry_usearch(|| index.reserve(new_capacity), "Failed to expand capacity")?;

        Ok(())
    }

    pub(crate) fn save_internal(&self, path: &Path) -> Result<()> {
        let _save_guard = self.save_lock.write();
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
        drop(index);
        drop(_save_guard);

        let mappings_path = path.with_extension("usearch.mappings");
        let file = File::create(&mappings_path).map_err(|e| {
            Error::Vector(VectorError::IndexError(format!(
                "Failed to create mappings file: {}",
                e
            )))
        })?;
        let mut writer = BufWriter::new(file);

        write_mappings_to_writer(&mut writer, mappings.into_iter(), count, &self.config)
    }

    pub(crate) fn convert_matches(&self, matches: Matches) -> Vec<(NodeId, f32)> {
        let mut results: Vec<(NodeId, f32)> = Vec::with_capacity(matches.keys.len());

        for (key, distance) in matches.keys.iter().zip(matches.distances.iter()) {
            if let Some(node_id_ref) = self.reverse_mapping.get(key) {
                let node_id = *node_id_ref.value();
                let similarity = match self.config.metric {
                    DistanceMetric::Cosine => {
                        let sim = 1.0 - distance;
                        if sim.is_nan() {
                            0.0
                        } else {
                            sim.clamp(-1.0, 1.0)
                        }
                    }
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

    /// Get the number of mappings in the ID map.
    pub fn len_mappings(&self) -> usize {
        self.id_mapping.len()
    }
}

impl VectorIndex for HnswIndex {
    /// Adds a vector to the index.
    ///
    /// # Concurrency
    ///
    /// This method is thread-safe and supports high concurrency.
    /// - Uses sharded locking based on `id` to allow concurrent updates to different nodes.
    /// - Only blocks other writers to the *same* node (or colliding lock shard).
    /// - Readers (`search`) are generally not blocked, except during rare capacity expansions.
    ///
    /// # Errors
    ///
    /// - `DimensionMismatch` if vector length doesn't match index configuration.
    /// - `IndexError` if called on a read-only memory-mapped index.
    /// - `IndexError` if called recursively from within a `search_with_filter` callback (deadlock prevention).
    fn add(&self, id: NodeId, vector: &[f32]) -> Result<()> {
        if IN_FILTER_CALLBACK.with(|flag| flag.get()) {
            return Err(Error::Vector(VectorError::IndexError(
                "Cannot modify index from within a search_with_filter callback. \
                 This would cause a deadlock due to lock re-entrancy. \
                 Consider collecting modifications and applying them after the search completes."
                    .to_string(),
            )));
        }

        if self.is_mmap {
            return Err(Error::Vector(VectorError::IndexError(
                "Cannot modify memory-mapped index (read-only)".to_string(),
            )));
        }

        validate_vector(vector)?;

        if vector.len() != self.config.dimensions {
            return Err(Error::Vector(VectorError::DimensionMismatch {
                expected: self.config.dimensions,
                actual: vector.len(),
            }));
        }

        self.maybe_block_in_place(|| {
            self.check_and_expand_capacity(1)?;

            let lock_idx = (id.as_u64() as usize) % self.entry_locks.len();
            let _key_guard = self.entry_locks[lock_idx].lock();
            let _save_guard = self.save_lock.read();

            match self.id_mapping.entry(id) {
                dashmap::mapref::entry::Entry::Occupied(entry) => {
                    let existing_key = *entry.get();
                    drop(entry);

                    #[cfg(test)]
                    {
                        if let Some(hook) = TEST_RACE_HOOK.with(|h| h.get()) {
                            hook(self, id);
                        }
                    }

                    let index = self.inner.write();

                    if let Some(current_entry) = self.id_mapping.get(&id) {
                        if *current_entry != existing_key {
                            return Err(Error::Vector(VectorError::IndexError(
                                "Concurrent modification detected during update (mapping changed)"
                                    .to_string(),
                            )));
                        }
                    } else {
                        return Err(Error::Vector(VectorError::IndexError(
                            "Concurrent modification detected during update (node removed)"
                                .to_string(),
                        )));
                    }

                    if index.contains(existing_key) {
                        self.retry_usearch(
                            || index.remove(existing_key),
                            "Failed to remove existing vector",
                        )?;
                    } else if index.size() >= index.capacity() {
                        let new_capacity = (index.capacity() * 2).max(1024);
                        self.retry_usearch(
                            || index.reserve(new_capacity),
                            "Failed to expand capacity (race recovery)",
                        )?;
                    }

                    if let Err(e) = self
                        .retry_usearch(|| index.add(existing_key, vector), "Failed to add vector")
                    {
                        if e.to_string().contains("Duplicate keys") {
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
                    const MAX_VALID_KEY: u64 = u64::MAX - 1000;
                    drop(entry);

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

                    let index = self.inner.write();

                    if index.size() >= index.capacity() {
                        let new_capacity = (index.capacity() * 2).max(1024);
                        self.retry_usearch(
                            || index.reserve(new_capacity),
                            "Failed to expand capacity (race recovery)",
                        )?;
                    }

                    self.retry_usearch(|| index.add(key, vector), "Failed to add vector")?;

                    #[cfg(test)]
                    {
                        if let Some(hook) = TEST_RACE_HOOK.with(|h| h.get()) {
                            hook(self, id);
                        }
                    }

                    let race_detected = match self.id_mapping.entry(id) {
                        dashmap::mapref::entry::Entry::Occupied(_) => true,
                        dashmap::mapref::entry::Entry::Vacant(e) => {
                            e.insert(key);
                            false
                        }
                    };

                    if race_detected {
                        self.retry_usearch(
                            || index.remove(key),
                            "Failed to rollback vector after concurrent add",
                        )?;
                        return Err(Error::Vector(VectorError::IndexError(
                            "Concurrent add detected for same NodeId, vector already exists"
                                .to_string(),
                        )));
                    }

                    self.reverse_mapping.insert(key, id);
                    self.stats.vectors_added.fetch_add(1, Ordering::Relaxed);
                    drop(index);
                    Ok(())
                }
            }
        })
    }

    /// Removes a vector from the index.
    ///
    /// # Concurrency
    ///
    /// Thread-safe. Uses the same sharded locking mechanism as `add`.
    ///
    /// # Errors
    ///
    /// - `IndexError` if called on a read-only memory-mapped index.
    /// - `IndexError` if called recursively from within a `search_with_filter` callback.
    fn remove(&self, id: NodeId) -> Result<()> {
        if IN_FILTER_CALLBACK.with(|flag| flag.get()) {
            return Err(Error::Vector(VectorError::IndexError(
                "Cannot modify index from within a search_with_filter callback. \
                 This would cause a deadlock due to lock re-entrancy. \
                 Consider collecting modifications and applying them after the search completes."
                    .to_string(),
            )));
        }

        if self.is_mmap {
            return Err(Error::Vector(VectorError::IndexError(
                "Cannot modify memory-mapped index (read-only)".to_string(),
            )));
        }

        self.maybe_block_in_place(|| {
            let lock_idx = (id.as_u64() as usize) % self.entry_locks.len();
            let _key_guard = self.entry_locks[lock_idx].lock();
            let _save_guard = self.save_lock.read();

            if let Some((_, key)) = self.id_mapping.remove(&id) {
                self.reverse_mapping.remove(&key);
                let index = self.inner.write();
                self.retry_usearch(|| index.remove(key), "Failed to remove vector")?;
                self.stats.vectors_removed.fetch_add(1, Ordering::Relaxed);
            }
            Ok(())
        })
    }

    /// Searches for the k-nearest neighbors.
    ///
    /// # Concurrency
    ///
    /// Thread-safe and non-blocking. Multiple searches can proceed in parallel.
    /// Takes a read lock on the index structure.
    fn search(&self, query: &[f32], k: usize) -> Result<Vec<(NodeId, f32)>> {
        if IN_FILTER_CALLBACK.with(|flag| flag.get()) {
            return Err(Error::Vector(VectorError::IndexError(
                "Cannot perform search from within a search_with_filter callback. \
                 This prevents deadlocks when concurrent writers are pending."
                    .to_string(),
            )));
        }

        validate_vector(query)?;

        if query.len() != self.config.dimensions {
            return Err(Error::Vector(VectorError::DimensionMismatch {
                expected: self.config.dimensions,
                actual: query.len(),
            }));
        }

        let k_capped = k.min(self.max_k);

        self.maybe_block_in_place(|| {
            let matches = self.retry_usearch(
                || {
                    let index = self.inner.read();
                    index.search(query, k_capped)
                },
                "Search failed",
            )?;

            self.stats
                .searches_performed
                .fetch_add(1, Ordering::Relaxed);

            let results = self.convert_matches(matches);
            Ok(results)
        })
    }

    /// Searches for k-nearest neighbors that satisfy a predicate.
    ///
    /// # Implementation Details
    ///
    /// Uses an iterative expansion strategy:
    /// 1. Searches for `k` candidates.
    /// 2. Filters candidates using the predicate.
    /// 3. If fewer than `k` results remain, doubles the search radius and retries.
    /// 4. Repeats until `k` results are found or the index is exhausted.
    ///
    /// This ensures high recall even with restrictive filters, though performance
    /// degrades if the filter selectivity is very low (e.g., < 1%).
    ///
    /// # Deadlock Prevention
    ///
    /// The predicate is executed while holding a lock on the index. To prevent deadlocks,
    /// the predicate **must not** attempt to modify the index (add/remove) or perform
    /// another search. This is enforced by a thread-local flag (`IN_FILTER_CALLBACK`).
    fn search_with_filter<F>(
        &self,
        query: &[f32],
        k: usize,
        predicate: F,
    ) -> Result<Vec<(NodeId, f32)>>
    where
        F: Fn(&NodeId) -> bool + Send + Sync,
    {
        if IN_FILTER_CALLBACK.with(|flag| flag.get()) {
            return Err(Error::Vector(VectorError::IndexError(
                "Cannot perform search_with_filter from within a search_with_filter callback. \
                 This prevents deadlocks when concurrent writers are pending."
                    .to_string(),
            )));
        }

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

        self.maybe_block_in_place(|| {
            let mut candidate_k = k_capped.min(max_candidates);
            loop {
                let candidates = {
                    let matches = self.retry_usearch(
                        || {
                            let index = self.inner.read();
                            index.search(query, candidate_k)
                        },
                        "Filtered search failed",
                    )?;
                    self.convert_matches(matches)
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
        })
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

    /// Persists the index to disk.
    ///
    /// Saves two files:
    /// 1. `{path}`: The HNSW graph structure (handled by usearch).
    /// 2. `{path}.usearch.mappings`: The NodeId <-> internal key mapping.
    ///
    /// # Consistency
    ///
    /// Acquires a global write lock (`save_lock`) to ensure the index and mappings
    /// are saved in a consistent state. Blocks concurrent `add`/`remove` operations.
    fn save(&self, path: &Path) -> Result<()> {
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
        Ok(())
    }
}
