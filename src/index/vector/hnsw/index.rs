//! HNSW index implementation.

use crate::core::error::{Error, Result, VectorError};
use crate::core::id::NodeId;
use crate::core::property::MAX_VECTOR_DIMENSIONS;
use crate::core::vector::validate_vector;
use crate::index::vector::{DistanceMetric, Quantization, VectorIndex};
use dashmap::DashMap;
use parking_lot::{Mutex, RwLock};
use std::fs::File;
use std::io::BufWriter;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use usearch::{ffi::Matches, Index, IndexOptions, MetricKind, ScalarKind};

use super::config::{HnswConfig, IndexMetadata};
use super::storage::{load_mappings_with_integrity, write_mappings_to_writer};

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
    pub(crate) static TEST_RACE_HOOK: std::cell::Cell<Option<TestRaceHook>> = const { std::cell::Cell::new(None) };
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

/// Maximum number of results that can be requested in a search.
pub(crate) const MAX_K: usize = 100_000;

/// Number of sharded locks for entry updates.
pub(crate) const NUM_ENTRY_LOCKS: usize = 64;

/// Convert our DistanceMetric to usearch's MetricKind
pub(crate) fn to_usearch_metric(metric: DistanceMetric) -> MetricKind {
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
pub(crate) fn to_usearch_scalar(quantization: Quantization) -> ScalarKind {
    match quantization {
        Quantization::F32 => ScalarKind::F32,
        Quantization::F16 => ScalarKind::F16,
        Quantization::I8 => ScalarKind::I8,
    }
}

/// Statistics for index operations.
#[derive(Debug, Default)]
pub struct IndexStats {
    /// Total number of vectors added (including updates)
    pub vectors_added: AtomicU64,
    /// Total number of vectors removed
    pub vectors_removed: AtomicU64,
    /// Total number of search operations performed
    pub searches_performed: AtomicU64,
    /// Number of times search operations were retried due to transient errors
    pub search_retries: AtomicU64,
    /// Number of searches that failed even after all retry attempts
    pub search_retry_failures: AtomicU64,
}

/// Maximum number of search attempts (initial attempt + retries) when encountering transient errors.
const MAX_SEARCH_ATTEMPTS: u32 = 4; // 1 initial attempt + 3 retries

/// Check if a usearch error is transient and should be retried.
fn is_retryable_usearch_error(error_msg: &str) -> bool {
    // Thread pool exhaustion is a transient error that resolves when threads become available
    error_msg.contains("No available threads to lock")
}

// Helper to create the metric wrapper - extracted for testing
pub(crate) fn create_metric_wrapper<F>(
    dims: usize,
    distance_fn: Arc<F>,
) -> Box<dyn Fn(*const f32, *const f32) -> f32 + Send + Sync>
where
    F: Fn(&[f32], &[f32]) -> f32 + Send + Sync + 'static + ?Sized,
{
    Box::new(move |a: *const f32, b: *const f32| {
        // Check for null pointers to prevent UB
        if a.is_null() || b.is_null() {
            eprintln!("usearch passed null pointer to metric function - returning max distance");
            return f32::MAX;
        }

        // Check for alignment to prevent UB
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
pub struct HnswIndex {
    /// Underlying usearch index
    pub(crate) inner: Arc<RwLock<Index>>,
    /// Configuration used to create this index
    pub(crate) config: HnswConfig,
    /// ID mapping: NodeId -> usearch key (u64)
    pub(crate) id_mapping: Arc<DashMap<NodeId, u64>>,
    /// Reverse mapping: usearch key -> NodeId
    pub(crate) reverse_mapping: Arc<DashMap<u64, NodeId>>,
    /// Next available key
    pub(crate) next_key: AtomicU64,
    /// Statistics
    pub(crate) stats: Arc<IndexStats>,
    /// Maximum k for DoS protection
    pub(crate) max_k: usize,
    /// Whether this index is memory-mapped (read-only)
    pub(crate) is_mmap: bool,
    /// Lock to ensure consistency between index and mapping for saving.
    pub(crate) save_lock: Arc<RwLock<()>>,
    /// Sharded locks to serialize updates to the same key/node.
    pub(crate) entry_locks: Vec<Mutex<()>>,
}

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
                        "Concurrent modification detected during update (node removed)".to_string(),
                    )));
                }

                if index.contains(existing_key) {
                    self.retry_usearch(
                        || index.remove(existing_key),
                        "Failed to remove existing vector",
                    )?;
                } else {
                    if index.size() >= index.capacity() {
                        let new_capacity = (index.capacity() * 2).max(1024);
                        self.retry_usearch(
                            || index.reserve(new_capacity),
                            "Failed to expand capacity (race recovery)",
                        )?;
                    }
                }

                if let Err(e) =
                    self.retry_usearch(|| index.add(existing_key, vector), "Failed to add vector")
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
    }

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
    }

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

        let index = self.inner.read();

        for attempt in 0..MAX_SEARCH_ATTEMPTS {
            match index.search(query, k_capped) {
                Ok(matches) => {
                    self.stats
                        .searches_performed
                        .fetch_add(1, Ordering::Relaxed);

                    let results = self.convert_matches(matches);
                    return Ok(results);
                }
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
                        "Search failed: {}",
                        e
                    ))));
                }
            }
        }

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

        let mut candidate_k = k_capped.min(max_candidates);
        loop {
            let candidates = {
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

impl HnswIndex {
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

    fn check_and_expand_capacity(&self, vectors_to_add: usize) -> Result<()> {
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

    fn save_internal(&self, path: &Path) -> Result<()> {
        let _save_guard = self.save_lock.write();

        let index = self.inner.write();

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

    pub(crate) fn validate_metadata(
        metadata: Option<IndexMetadata>,
        config: &HnswConfig,
    ) -> Result<()> {
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

    fn convert_matches(&self, matches: Matches) -> Vec<(NodeId, f32)> {
        let mut results: Vec<(NodeId, f32)> = Vec::with_capacity(matches.keys.len());

        for (key, distance) in matches.keys.iter().zip(matches.distances.iter()) {
            if let Some(node_id_ref) = self.reverse_mapping.get(key) {
                let node_id = *node_id_ref.value();

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

    /// Returns the number of ID mappings.
    ///
    /// This is useful for consistency checks against `len()`.
    /// Ideally, `len() == len_mappings()`. If `len() > len_mappings()`,
    /// there are vectors in the index that cannot be retrieved (Zombie Vectors).
    pub fn len_mappings(&self) -> usize {
        self.id_mapping.len()
    }

    /// Creates a new in-memory HNSW index from a configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if the configuration is invalid (e.g., dimensions = 0).
    pub fn new(config: HnswConfig) -> Result<Self> {
        super::builder::HnswIndexBuilder::from_config(&config).build()
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

    /// Loads an index from disk.
    ///
    /// # Safety and Verification
    ///
    /// This method performs strict validation to ensure data integrity and safety:
    /// - **Dimensions Check**: Verifies that the on-disk index matches the configured dimensions.
    /// - **Metric Check**: Verifies that the on-disk metric matches the configuration.
    /// - **Integrity Check**: Verifies the CRC32 checksum of the mappings file.
    /// - **Security**: Checks against DoS vectors (huge files, invalid headers).
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The file does not exist or cannot be read.
    /// - The index configuration (dimensions, metric, quantization) does not match the file.
    /// - The mappings file is corrupted (CRC mismatch).
    /// - Custom metric is used with non-F32 quantization (safety violation).
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

    /// Opens a memory-mapped index from disk in read-only mode.
    ///
    /// This is useful for serving large indexes that exceed available RAM, as the OS
    /// will page in parts of the index as needed.
    ///
    /// # Limitations
    ///
    /// - **Read-Only**: `add` and `remove` operations will return `IndexError`.
    /// - **Performance**: Search latency may be higher than in-memory indexes due to disk I/O.
    ///
    /// # Errors
    ///
    /// Returns an error if the file is invalid or corrupted.
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

        Ok(HnswIndex {
            inner: Arc::new(RwLock::new(index)),
            config: HnswConfig {
                dimensions,
                m: connectivity,
                quantization,
                metric,
                storage: crate::index::vector::StorageMode::MemoryMapped {
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
unsafe impl Send for HnswIndex {}
unsafe impl Sync for HnswIndex {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::vector::hnsw::builder::HnswIndexBuilder;

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
        let index = HnswIndexBuilder::new(2, DistanceMetric::DotProduct).build()?;
        let node = NodeId::new(1).unwrap();
        index.add(node, &[1.0, 0.0])?;

        let results = index.search(&[1.0, 0.0], 1)?;
        assert_eq!(results.len(), 1);
        let similarity = results[0].1;

        assert!(
            (similarity - 1.0).abs() < 0.001,
            "Expected 1.0, got {}",
            similarity
        );

        Ok(())
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

    #[test]
    fn test_capacity_check_and_expand() {
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .initial_capacity(10)
            .build()
            .unwrap();

        assert_eq!(index.len(), 0);

        index.check_and_expand_capacity(1).unwrap();

        for i in 0..10 {
            let id = NodeId::new(i + 1).unwrap();
            index.add(id, &[1.0, 0.0, 0.0, 0.0]).unwrap();
        }

        assert_eq!(index.len(), 10);

        index.check_and_expand_capacity(1).unwrap();
    }

    #[test]
    fn test_vacant_path_race_recovery() -> Result<()> {
        TEST_SKIP_CAPACITY_CHECK.store(true, Ordering::SeqCst);
        struct ResetGuard;
        impl Drop for ResetGuard {
            fn drop(&mut self) {
                TEST_SKIP_CAPACITY_CHECK.store(false, Ordering::SeqCst);
            }
        }
        let _reset = ResetGuard;

        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .initial_capacity(10)
            .build()?;

        for i in 0..10 {
            index.add(NodeId::new(i + 1).unwrap(), &[1.0, 0.0, 0.0, 0.0])?;
        }
        assert_eq!(index.len(), 10);

        index.add(NodeId::new(11).unwrap(), &[1.0, 0.0, 0.0, 0.0])?;

        assert_eq!(index.len(), 11);
        assert!(index.inner.read().capacity() > 10);

        Ok(())
    }

    #[test]
    fn test_occupied_path_inconsistency_race_recovery() -> Result<()> {
        TEST_SKIP_CAPACITY_CHECK.store(true, Ordering::SeqCst);
        struct ResetGuard;
        impl Drop for ResetGuard {
            fn drop(&mut self) {
                TEST_SKIP_CAPACITY_CHECK.store(false, Ordering::SeqCst);
            }
        }
        let _reset = ResetGuard;

        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .initial_capacity(10)
            .build()?;

        for i in 0..10 {
            index.add(NodeId::new(i + 1).unwrap(), &[1.0, 0.0, 0.0, 0.0])?;
        }
        assert_eq!(index.len(), 10);

        let node_id = NodeId::new(1).unwrap();

        TEST_RACE_HOOK.with(|h| {
            h.set(Some(|idx, _id| {
                let index = idx.inner.write();
                let _ = index.remove(0);
                let _ = index.add(999, &[0.0, 1.0, 0.0, 0.0]);
            }))
        });

        index.add(node_id, &[0.0, 1.0, 0.0, 0.0])?;

        TEST_RACE_HOOK.with(|h| h.set(None));

        assert_eq!(index.len(), 11);
        assert!(index.inner.read().capacity() > 10);

        Ok(())
    }

    fn create_test_index() -> HnswIndex {
        HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap()
    }

    #[test]
    fn test_add_reentrancy_check() {
        let index = create_test_index();
        let node_id = NodeId::new(1).unwrap();
        let vec = vec![1.0, 0.0, 0.0, 0.0];

        let _guard = FilterCallbackGuard::new();

        let result = index.add(node_id, &vec);
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::IndexError(msg))) => {
                assert!(
                    msg.contains("Cannot modify index from within a search_with_filter callback")
                );
            }
            _ => panic!("Expected re-entrancy error"),
        }
    }

    #[test]
    fn test_remove_reentrancy_check() {
        let index = create_test_index();
        let node_id = NodeId::new(1).unwrap();

        let _guard = FilterCallbackGuard::new();

        let result = index.remove(node_id);
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::IndexError(msg))) => {
                assert!(
                    msg.contains("Cannot modify index from within a search_with_filter callback")
                );
            }
            _ => panic!("Expected re-entrancy error"),
        }
    }

    #[test]
    fn test_save_reentrancy_check() {
        let index = create_test_index();
        let path = Path::new("dummy.index");

        let _guard = FilterCallbackGuard::new();

        let result = index.save(path);
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::IndexError(msg))) => {
                assert!(
                    msg.contains("Cannot save index from within a search_with_filter callback")
                );
            }
            _ => panic!("Expected re-entrancy error"),
        }
    }

    #[test]
    fn test_search_reentrancy_check() {
        let index = create_test_index();
        let query = vec![1.0, 0.0, 0.0, 0.0];

        let _guard = FilterCallbackGuard::new();

        let result = index.search(&query, 10);
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::IndexError(msg))) => {
                assert!(
                    msg.contains("Cannot perform search from within a search_with_filter callback")
                );
            }
            _ => panic!("Expected re-entrancy error"),
        }
    }

    #[test]
    fn test_search_with_filter_reentrancy_check() {
        let index = create_test_index();
        let query = vec![1.0, 0.0, 0.0, 0.0];

        let _guard = FilterCallbackGuard::new();

        let result = index.search_with_filter(&query, 10, |_| true);
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::IndexError(msg))) => {
                assert!(msg.contains(
                    "Cannot perform search_with_filter from within a search_with_filter callback"
                ));
            }
            _ => panic!("Expected re-entrancy error"),
        }
    }

    #[test]
    fn test_index_stats_default() {
        let stats = IndexStats::default();
        assert_eq!(
            stats
                .vectors_added
                .load(std::sync::atomic::Ordering::Relaxed),
            0
        );
    }

    #[test]
    fn test_retry_usearch_logic() {
        let index = create_test_index();
        let mut attempts = 0;

        let result: crate::core::error::Result<()> = index.retry_usearch(
            || {
                attempts += 1;
                Err("No available threads to lock".to_string())
            },
            "test_context",
        );

        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::IndexError(msg))) => {
                assert!(msg.contains("test_context"));
                assert!(msg.contains("No available threads to lock"));
            }
            _ => panic!("Expected IndexError"),
        }

        assert_eq!(attempts, 4);

        assert_eq!(index.stats.search_retries.load(Ordering::Relaxed), 3);
        assert_eq!(index.stats.search_retry_failures.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_retry_usearch_success_after_retry() {
        let index = create_test_index();
        let mut attempts = 0;

        let result: crate::core::error::Result<()> = index.retry_usearch(
            || {
                attempts += 1;
                if attempts < 3 {
                    Err("No available threads to lock".to_string())
                } else {
                    Ok(())
                }
            },
            "test_context",
        );

        assert!(result.is_ok());
        assert_eq!(attempts, 3);

        assert_eq!(index.stats.search_retries.load(Ordering::Relaxed), 2);
        assert_eq!(index.stats.search_retry_failures.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_save_async_context() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("async_save.index");

        let index = create_test_index();
        index
            .add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0])
            .unwrap();

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();

        runtime.block_on(async {
            let result = index.save(&path);
            assert!(result.is_ok());
        });

        assert!(path.exists());
        assert!(path.with_extension("usearch.mappings").exists());
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
}
