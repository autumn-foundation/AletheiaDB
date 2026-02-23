//! HNSW (Hierarchical Navigable Small World) vector index implementation.
//!
//! This module provides a wrapper around the `usearch` library's HNSW index,
//! implementing the `VectorIndex` trait for approximate k-nearest neighbor search.
//!
//! # Overview
//!
//! HNSW is a graph-based algorithm for approximate nearest neighbor search that
//! provides excellent search performance with logarithmic average-case complexity.
//!
//! # Thread Safety
//!
//! `HnswIndex` is fully thread-safe for concurrent operations.
//!
//! # Concurrency Model
//!
//! To ensure thread safety and prevent deadlocks, the index uses a strict lock hierarchy:
//!
//! 1. **Entry Locks** (`entry_locks`): Sharded mutexes protecting updates to specific keys.
//! 2. **Save Lock** (`save_lock`): Read/Write lock.
//! 3. **Inner Index Lock** (`inner`): Read/Write lock protecting the usearch index.
//! 4. **ID Mapping** (`id_mapping`): Concurrent `DashMap`.
//!
//! **Invariant**: Always acquire locks in the order: `entry_locks` → `save_lock` → `inner` → `id_mapping`.

pub(crate) mod config;
pub(crate) mod persistence;
pub(crate) mod stats;
#[cfg(test)]
mod tests;
pub(crate) mod utils;

pub use config::{HnswConfig, HnswIndexBuilder};
use persistence::{load_index, open_mmap_index, save_index};
use stats::IndexStats;
use utils::{FilterCallbackGuard, IN_FILTER_CALLBACK, is_retryable_usearch_error};

#[cfg(test)]
use utils::{TEST_RACE_HOOK, TEST_SKIP_CAPACITY_CHECK};

use crate::core::error::{Error, Result, VectorError};
use crate::core::id::NodeId;
use crate::core::vector::validate_vector;
use crate::index::vector::{DistanceMetric, Quantization, VectorIndex};
use dashmap::DashMap;
use parking_lot::{Mutex, RwLock};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use usearch::{Index, ffi::Matches};

/// Maximum number of results that can be requested in a search.
const MAX_K: usize = 100_000;

/// Number of sharded locks for entry updates.
const NUM_ENTRY_LOCKS: usize = 64;

/// Maximum number of search attempts (initial attempt + retries) when encountering transient errors.
const MAX_SEARCH_ATTEMPTS: u32 = 4; // 1 initial attempt + 3 retries

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
    max_k: usize,
    /// Whether this index is memory-mapped (read-only)
    is_mmap: bool,
    /// Lock to ensure consistency between index and mapping for saving.
    pub(crate) save_lock: Arc<RwLock<()>>,
    /// Sharded locks to serialize updates to the same key/node.
    entry_locks: Vec<Mutex<()>>,
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
                 This would cause a deadlock due to lock re-entrancy."
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
                } else if index.size() >= index.capacity() {
                    let new_capacity = (index.capacity() * 2).max(1024);
                    self.retry_usearch(
                        || index.reserve(new_capacity),
                        "Failed to expand capacity (race recovery)",
                    )?;
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
                "Cannot modify index from within a search_with_filter callback.".to_string(),
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
                "Cannot perform search from within a search_with_filter callback.".to_string(),
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
                "Cannot perform search_with_filter from within a search_with_filter callback."
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
                "Cannot save index from within a search_with_filter callback.".to_string(),
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

// Private helper methods for HnswIndex
impl HnswIndex {
    fn save_internal(&self, path: &Path) -> Result<()> {
        // Acquire save_lock (exclusive) to prevent concurrent adds/removes.
        let _save_guard = self.save_lock.write();
        let index = self.inner.read();

        // Delegate to persistence module
        save_index(&index, &self.id_mapping, &self.config, path)
    }

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
        let components = load_index(path, config.clone())?;

        Ok(HnswIndex {
            inner: Arc::new(RwLock::new(components.index)),
            config,
            id_mapping: Arc::new(components.id_mapping),
            reverse_mapping: Arc::new(components.reverse_mapping),
            next_key: AtomicU64::new(components.max_key + 1),
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
        let components = open_mmap_index(path)?;

        Ok(HnswIndex {
            inner: Arc::new(RwLock::new(components.index)),
            config: components.config,
            id_mapping: Arc::new(components.id_mapping),
            reverse_mapping: Arc::new(components.reverse_mapping),
            next_key: AtomicU64::new(components.max_key + 1),
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
