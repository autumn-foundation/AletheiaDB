//! Sparse vector index implementation using an inverted index.
//!
//! This module provides efficient indexing and search for sparse vectors, which are
//! vectors where most values are zero. Common use cases include:
//!
//! - **BM25**: Traditional text retrieval scoring
//! - **SPLADE**: Sparse learned embeddings for semantic search
//! - **TF-IDF**: Term frequency-inverse document frequency vectors
//! - **One-hot encodings**: Categorical feature vectors
//!
//! # Architecture
//!
//! The index uses an **inverted index** structure:
//! - For each non-zero dimension, maintains a posting list of (NodeId, value) pairs
//! - Search iterates only over dimensions present in the query vector
//! - Achieves O(nnz_query * avg_posting_size) complexity instead of O(n * d)
//!
//! # Features
//!
//! - **Multiple scoring methods**: Dot product, cosine similarity, BM25
//! - **Thread-safe**: Concurrent reads and writes via interior mutability
//! - **Memory-efficient**: Only stores non-zero values
//! - **Persistence**: Save/load support (planned, not yet implemented)
//!
//! # Example
//!
//! ```rust,no_run
//! use aletheiadb::index::vector::sparse::{SparseVectorIndex, SparseIndexConfig, ScoringMethod};
//! use aletheiadb::core::id::NodeId;
//! use aletheiadb::core::vector::SparseVec;
//!
//! # fn example() -> aletheiadb::core::error::Result<()> {
//! // Create an index for 10,000-dimensional sparse vectors
//! let index = SparseVectorIndex::new(SparseIndexConfig {
//!     dimensions: 10_000,
//!     scoring: ScoringMethod::DotProduct,
//!     ..Default::default()
//! })?;
//!
//! // Add a sparse document vector (only 3 non-zero terms out of 10,000)
//! let doc = SparseVec::new(vec![42, 100, 5000], vec![2.5, 1.8, 3.2], 10_000)?;
//! index.add(NodeId::new(1).unwrap(), &doc)?;
//!
//! // Search for similar documents
//! let query = SparseVec::new(vec![42, 200], vec![1.0, 2.0], 10_000)?;
//! let results = index.search(&query, 10)?;
//! # Ok(())
//! # }
//! ```
//!
//! # Performance
//!
//! For typical sparse vectors (nnz < 100 in 10K+ dimensional space):
//! - **Add**: O(nnz) per vector
//! - **Search**: O(nnz_query * avg_posting_length * log k) for top-k
//! - **Memory**: O(total_nnz) where total_nnz is sum of all vectors' non-zeros
//!
//! Compared to dense vector indexes (HNSW):
//! - Much more memory-efficient for truly sparse data
//! - Faster search when sparsity is high (>99%)
//! - No approximation - exact similarity scores

use crate::core::error::{Error, Result, VectorError};
use crate::core::hasher::BuildIdentityHasher;
use crate::core::id::NodeId;
use crate::core::property::MAX_VECTOR_DIMENSIONS;
use crate::core::vector::SparseVec;
use bitcode::{Decode, Encode};
use crc32fast::Hasher;
use dashmap::DashMap;
use parking_lot::Mutex;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

/// Maximum number of results that can be requested in a search.
///
/// This prevents DoS attacks via excessive memory allocation.
const MAX_K: usize = 100_000;

/// Magic bytes for sparse index files: "GSPS"
const SPARSE_INDEX_MAGIC: [u8; 4] = [0x47, 0x53, 0x50, 0x53];

/// Current format version for sparse index persistence
const SPARSE_INDEX_VERSION: u16 = 1;

/// Scoring method for sparse vector similarity.
///
/// Different scoring methods are suitable for different use cases:
///
/// - **DotProduct**: Raw inner product, suitable for pre-normalized vectors
/// - **Cosine**: Angle-based similarity, ignores magnitude
/// - **BM25**: Best for text retrieval with term frequencies
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[cfg_attr(feature = "config-toml", derive(serde::Serialize, serde::Deserialize))]
pub enum ScoringMethod {
    /// Dot product (inner product) similarity.
    /// Scores can be any real number. Higher is more similar.
    #[default]
    DotProduct,
    /// Cosine similarity (normalized dot product).
    /// Scores are in range [-1, 1]. Higher is more similar.
    Cosine,
    /// BM25 scoring for text retrieval.
    /// Uses term frequency saturation and document length normalization.
    BM25 {
        /// Controls term frequency saturation (typical: 1.2-2.0)
        k1: f32,
        /// Controls document length normalization (typical: 0.75)
        b: f32,
    },
}

impl ScoringMethod {
    /// Creates BM25 scoring with default parameters (k1=1.5, b=0.75).
    pub fn bm25_default() -> Self {
        ScoringMethod::BM25 { k1: 1.5, b: 0.75 }
    }
}

/// Configuration for sparse vector index.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "config-toml", derive(serde::Serialize, serde::Deserialize))]
pub struct SparseIndexConfig {
    /// Vector dimensionality (total dimensions including zeros).
    pub dimensions: usize,
    /// Scoring method for similarity computation.
    pub scoring: ScoringMethod,
    /// Initial capacity hint for the number of vectors.
    pub initial_capacity: usize,
}

impl Default for SparseIndexConfig {
    fn default() -> Self {
        SparseIndexConfig {
            dimensions: 0,
            scoring: ScoringMethod::default(),
            initial_capacity: 1000,
        }
    }
}

impl SparseIndexConfig {
    /// Creates a new configuration with the specified dimensions.
    pub fn new(dimensions: usize) -> Self {
        SparseIndexConfig {
            dimensions,
            ..Default::default()
        }
    }

    /// Sets the scoring method.
    pub fn with_scoring(mut self, scoring: ScoringMethod) -> Self {
        self.scoring = scoring;
        self
    }

    /// Sets the initial capacity hint.
    pub fn with_capacity(mut self, capacity: usize) -> Self {
        self.initial_capacity = capacity;
        self
    }
}

/// A posting in the inverted index: (node_id, value).
#[derive(Debug, Clone)]
struct Posting {
    node_id: NodeId,
    value: f32,
}

/// Entry in the score heap for top-k selection.
///
/// Uses `f32::total_cmp` for consistent ordering that handles NaN values correctly.
#[derive(Debug, Clone)]
struct ScoreEntry {
    node_id: NodeId,
    score: f32,
}

impl PartialEq for ScoreEntry {
    fn eq(&self, other: &Self) -> bool {
        // Use total_cmp for consistent equality that handles NaN
        self.score.total_cmp(&other.score) == Ordering::Equal && self.node_id == other.node_id
    }
}

impl Eq for ScoreEntry {}

impl PartialOrd for ScoreEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ScoreEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        // Min-heap: reverse order so we can pop the smallest
        // Use total_cmp for consistent ordering that handles NaN
        other.score.total_cmp(&self.score)
    }
}

/// Stored sparse vector with precomputed metadata.
#[derive(Debug, Clone)]
struct StoredVector {
    /// The sparse vector data
    vector: Arc<SparseVec>,
    /// Precomputed magnitude (L2 norm) for cosine similarity
    magnitude: f32,
}

/// Sparse vector index using an inverted index structure.
///
/// This index is optimized for high-dimensional sparse vectors where most
/// elements are zero. It uses an inverted index to efficiently compute
/// similarities by only iterating over non-zero elements.
///
/// # Thread Safety
///
/// This index is fully thread-safe for concurrent reads and writes.
/// Multiple threads can search simultaneously. Write operations (add/remove)
/// are serialized via an internal lock to ensure consistency between the
/// forward index, inverted index, and statistics.
pub struct SparseVectorIndex {
    /// Configuration
    config: SparseIndexConfig,
    /// Inverted index: dimension -> list of (node_id, value) postings
    inverted_index: DashMap<u32, Vec<Posting>, BuildIdentityHasher>,
    /// Forward index: node_id -> stored vector (for removal and updates)
    vectors: DashMap<NodeId, StoredVector, BuildIdentityHasher>,
    /// Number of vectors in the index
    count: AtomicUsize,
    /// Sum of all vector lengths (for BM25 avgdl)
    total_length: AtomicUsize,
    /// Document frequency: dimension -> count of documents containing it
    doc_freq: DashMap<u32, usize, BuildIdentityHasher>,
    /// Write lock to ensure atomicity of add/remove operations
    write_lock: Mutex<()>,
}

impl SparseVectorIndex {
    /// Creates a new sparse vector index with the given configuration.
    ///
    /// # Arguments
    ///
    /// * `config` - Index configuration
    ///
    /// # Returns
    ///
    /// A new sparse vector index.
    ///
    /// # Errors
    ///
    /// - Returns an error if dimensions is 0.
    /// - Returns an error if dimensions exceeds `MAX_VECTOR_DIMENSIONS`.
    pub fn new(config: SparseIndexConfig) -> Result<Self> {
        if config.dimensions == 0 {
            return Err(Error::Vector(VectorError::InvalidVector {
                reason: "Dimensions must be greater than 0".to_string(),
            }));
        }

        if config.dimensions > MAX_VECTOR_DIMENSIONS {
            return Err(Error::Vector(VectorError::DimensionTooLarge {
                dimension: config.dimensions,
                max_allowed: MAX_VECTOR_DIMENSIONS,
            }));
        }

        let capacity = config.initial_capacity;
        Ok(SparseVectorIndex {
            config,
            inverted_index: DashMap::with_capacity_and_hasher(capacity, BuildIdentityHasher),
            vectors: DashMap::with_capacity_and_hasher(capacity, BuildIdentityHasher),
            count: AtomicUsize::new(0),
            total_length: AtomicUsize::new(0),
            doc_freq: DashMap::with_capacity_and_hasher(capacity, BuildIdentityHasher),
            write_lock: Mutex::new(()),
        })
    }

    /// Adds a sparse vector to the index.
    ///
    /// If a vector with the same NodeId already exists, it will be replaced.
    ///
    /// # Arguments
    ///
    /// * `id` - The node ID to associate with this vector
    /// * `vector` - The sparse vector to add
    ///
    /// # Returns
    ///
    /// `Ok(())` if successful, or an error if validation fails.
    ///
    /// # Errors
    ///
    /// Returns `VectorError::DimensionMismatch` if the vector's dimension
    /// doesn't match the index's configured dimensions.
    pub fn add(&self, id: NodeId, vector: &SparseVec) -> Result<()> {
        // Validate dimensions before acquiring lock
        if vector.dimension() != self.config.dimensions {
            return Err(Error::Vector(VectorError::DimensionMismatch {
                expected: self.config.dimensions,
                actual: vector.dimension(),
            }));
        }

        // Acquire write lock to ensure atomicity of the entire add operation
        let _guard = self.write_lock.lock();

        // Remove existing vector if present (already holds lock)
        self.remove_internal_unlocked(id);

        // Store the vector
        let magnitude = vector.magnitude();
        let stored = StoredVector {
            vector: Arc::new(vector.clone()),
            magnitude,
        };

        // Add to forward index
        self.vectors.insert(id, stored);

        // Add to inverted index
        for (&dim, &val) in vector.indices().iter().zip(vector.values().iter()) {
            self.inverted_index.entry(dim).or_default().push(Posting {
                node_id: id,
                value: val,
            });

            // Update document frequency
            *self.doc_freq.entry(dim).or_insert(0) += 1;
        }

        // Update statistics with Release ordering to ensure all data modifications
        // are visible to other threads that observe the updated count
        self.total_length
            .fetch_add(vector.nnz(), AtomicOrdering::Relaxed);
        // Final count update uses Release to synchronize with Acquire loads
        self.count.fetch_add(1, AtomicOrdering::Release);

        Ok(())
    }

    /// Removes a vector from the index by node ID.
    ///
    /// # Arguments
    ///
    /// * `id` - The node ID to remove
    ///
    /// # Returns
    ///
    /// `Ok(())` if the vector was removed or didn't exist.
    pub fn remove(&self, id: NodeId) -> Result<()> {
        // Acquire write lock to ensure atomicity
        let _guard = self.write_lock.lock();
        self.remove_internal_unlocked(id);
        Ok(())
    }

    /// Internal removal that assumes write lock is already held.
    /// Returns whether a vector was actually removed.
    fn remove_internal_unlocked(&self, id: NodeId) -> bool {
        if let Some((_, stored)) = self.vectors.remove(&id) {
            let vec = &stored.vector;

            // Remove from inverted index
            for &dim in vec.indices() {
                if let Some(mut postings) = self.inverted_index.get_mut(&dim) {
                    postings.retain(|p| p.node_id != id);
                }

                // Update document frequency
                if let Some(mut freq) = self.doc_freq.get_mut(&dim) {
                    *freq = freq.saturating_sub(1);
                }
            }

            // Update statistics with Release ordering to ensure all data modifications
            // are visible to other threads that observe the updated count
            self.total_length
                .fetch_sub(vec.nnz(), AtomicOrdering::Relaxed);
            // Final count update uses Release to synchronize with Acquire loads
            self.count.fetch_sub(1, AtomicOrdering::Release);

            true
        } else {
            false
        }
    }

    /// Searches for the k most similar vectors to the query.
    ///
    /// # Arguments
    ///
    /// * `query` - The query sparse vector
    /// * `k` - Maximum number of results to return
    ///
    /// # Returns
    ///
    /// A vector of (NodeId, score) pairs, sorted by score descending.
    /// Scores interpretation depends on the scoring method.
    ///
    /// # Errors
    ///
    /// Returns an error if the query dimensions don't match.
    #[must_use = "search results should be used"]
    pub fn search(&self, query: &SparseVec, k: usize) -> Result<Vec<(NodeId, f32)>> {
        self.search_with_filter(query, k, |_| true)
    }

    /// Searches with a filter predicate.
    ///
    /// # Arguments
    ///
    /// * `query` - The query sparse vector
    /// * `k` - Maximum number of results to return
    /// * `predicate` - Filter function that returns true for nodes to include
    ///
    /// # Returns
    ///
    /// A vector of (NodeId, score) pairs where predicate returned true.
    #[must_use = "search results should be used"]
    pub fn search_with_filter<F>(
        &self,
        query: &SparseVec,
        k: usize,
        predicate: F,
    ) -> Result<Vec<(NodeId, f32)>>
    where
        F: Fn(&NodeId) -> bool + Send + Sync,
    {
        // Validate dimensions
        if query.dimension() != self.config.dimensions {
            return Err(Error::Vector(VectorError::DimensionMismatch {
                expected: self.config.dimensions,
                actual: query.dimension(),
            }));
        }

        // Cap k to prevent DoS
        let k = k.min(MAX_K);

        if k == 0 || self.is_empty() {
            return Ok(Vec::new());
        }

        // Accumulate scores for each candidate
        // For cosine similarity, we track magnitudes to avoid second lookups
        // For BM25, we track document lengths to avoid second lookups
        let is_cosine = matches!(self.config.scoring, ScoringMethod::Cosine);
        let mut scores: HashMap<NodeId, f32, BuildIdentityHasher> =
            HashMap::with_hasher(BuildIdentityHasher);
        // Magnitudes map is only used for cosine, but we always create it (cheap)
        let mut magnitudes: HashMap<NodeId, f32, BuildIdentityHasher> =
            HashMap::with_hasher(BuildIdentityHasher);
        // Document lengths map is only used for BM25, but we always create it (cheap)
        let mut doc_lengths: HashMap<NodeId, f32, BuildIdentityHasher> =
            HashMap::with_hasher(BuildIdentityHasher);
        let query_magnitude = query.magnitude();
        // Use Acquire ordering to synchronize with Release stores, ensuring we see
        // all data modifications that happened before the count was updated
        let n = self.count.load(AtomicOrdering::Acquire) as f32;
        let avgdl = if n > 0.0 {
            self.total_length.load(AtomicOrdering::Acquire) as f32 / n
        } else {
            1.0
        };

        // Iterate over query dimensions
        for (&dim, &query_val) in query.indices().iter().zip(query.values().iter()) {
            if let Some(postings) = self.inverted_index.get(&dim) {
                let df = self.doc_freq.get(&dim).map(|v| *v).unwrap_or(0) as f32;
                // IDF calculation with defensive bounds checking. In rare race conditions
                // df could exceed n, which would make the log argument < 1 and produce
                // negative IDF. We clamp to 0.0 to prevent negative scores.
                let idf = if df > 0.0 && n > 0.0 {
                    ((n - df + 0.5) / (df + 0.5) + 1.0).ln().max(0.0)
                } else {
                    0.0
                };

                for posting in postings.iter() {
                    if !predicate(&posting.node_id) {
                        continue;
                    }

                    let score_delta = match self.config.scoring {
                        ScoringMethod::DotProduct => query_val * posting.value,
                        ScoringMethod::Cosine => {
                            // Accumulate unnormalized dot product, normalize at the end
                            // Cache magnitude on first encounter to avoid second lookup
                            if !magnitudes.contains_key(&posting.node_id)
                                && let Some(stored) = self.vectors.get(&posting.node_id)
                            {
                                magnitudes.insert(posting.node_id, stored.magnitude);
                            }
                            query_val * posting.value
                        }
                        ScoringMethod::BM25 { k1, b } => {
                            // Cache document length on first encounter to avoid repeated lookups
                            let dl = *doc_lengths.entry(posting.node_id).or_insert_with(|| {
                                self.vectors
                                    .get(&posting.node_id)
                                    .map(|v| v.vector.nnz() as f32)
                                    .unwrap_or(1.0)
                            });

                            // BM25 term score
                            let tf = posting.value;
                            let numerator = tf * (k1 + 1.0);
                            let denominator = tf + k1 * (1.0 - b + b * dl / avgdl);
                            idf * numerator / denominator * query_val
                        }
                    };

                    *scores.entry(posting.node_id).or_insert(0.0) += score_delta;
                }
            }
        }

        // Normalize cosine scores using cached magnitudes
        if is_cosine && query_magnitude > 0.0 {
            for (&node_id, score) in scores.iter_mut() {
                if let Some(&mag) = magnitudes.get(&node_id)
                    && mag > 0.0
                {
                    *score /= query_magnitude * mag;
                }
            }
        }

        // Select top-k using min-heap
        let mut heap: BinaryHeap<ScoreEntry> = BinaryHeap::with_capacity(k + 1);

        for (node_id, score) in scores {
            heap.push(ScoreEntry { node_id, score });
            if heap.len() > k {
                heap.pop();
            }
        }

        // Convert to sorted results (highest score first)
        let mut results: Vec<(NodeId, f32)> =
            heap.into_iter().map(|e| (e.node_id, e.score)).collect();
        // Use total_cmp for consistent ordering that handles NaN
        results.sort_by(|a, b| b.1.total_cmp(&a.1));

        Ok(results)
    }

    /// Returns the number of vectors in the index.
    #[must_use]
    pub fn len(&self) -> usize {
        self.count.load(AtomicOrdering::Acquire)
    }

    /// Returns true if the index is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns the configured dimensions.
    #[must_use]
    pub fn dimensions(&self) -> usize {
        self.config.dimensions
    }

    /// Returns the scoring method.
    #[must_use]
    pub fn scoring(&self) -> ScoringMethod {
        self.config.scoring
    }

    /// Returns the configuration.
    #[must_use]
    pub fn config(&self) -> &SparseIndexConfig {
        &self.config
    }

    /// Checks if a node ID exists in the index.
    #[must_use]
    pub fn contains(&self, id: NodeId) -> bool {
        self.vectors.contains_key(&id)
    }

    /// Gets the sparse vector for a node ID, if it exists.
    #[must_use]
    pub fn get(&self, id: NodeId) -> Option<Arc<SparseVec>> {
        self.vectors.get(&id).map(|v| Arc::clone(&v.vector))
    }

    /// Returns approximate memory usage in bytes.
    ///
    /// This is an estimate based on:
    /// - ~16 bytes per posting in the inverted index
    /// - ~48 bytes overhead per stored vector plus 8 bytes per non-zero element
    ///
    /// Note: This does not account for DashMap internal overhead, Arc allocations,
    /// or memory fragmentation. Actual memory usage may be 20-50% higher.
    #[must_use]
    pub fn memory_usage(&self) -> usize {
        // Estimate: each posting is ~16 bytes, each stored vector is ~48 bytes + nnz*8
        let posting_size = 16;
        let vector_overhead = 48;
        let element_size = 8;

        let mut total = 0;

        // Inverted index
        for entry in self.inverted_index.iter() {
            total += entry.value().len() * posting_size;
        }

        // Forward index
        for entry in self.vectors.iter() {
            total += vector_overhead + entry.value().vector.nnz() * element_size;
        }

        total
    }

    /// Saves the index to a file.
    ///
    /// The file format is:
    /// - 4 bytes: Magic bytes "GSPS"
    /// - 2 bytes: Format version (little-endian u16)
    /// - N bytes: Bitcode-encoded index data
    /// - 4 bytes: CRC32 checksum of all preceding bytes
    ///
    /// # Errors
    ///
    /// Returns an error if serialization or I/O fails.
    pub fn save(&self, path: &Path) -> Result<()> {
        // Acquire read lock to ensure consistent snapshot
        let _guard = self.write_lock.lock();

        // Collect all vectors
        let mut vectors = Vec::with_capacity(self.len());
        for entry in self.vectors.iter() {
            let node_id = entry.key();
            let stored = entry.value();
            vectors.push(PersistedSparseVector {
                node_id: node_id.as_u64(),
                indices: stored.vector.indices().to_vec(),
                values: stored.vector.values().to_vec(),
            });
        }

        // Collect document frequencies
        let doc_freq: Vec<(u32, u64)> = self
            .doc_freq
            .iter()
            .map(|entry| (*entry.key(), *entry.value() as u64))
            .collect();

        // Build the data structure
        let data = SparseIndexData {
            dimensions: self.config.dimensions as u32,
            scoring: self.config.scoring.into(),
            count: self.count.load(AtomicOrdering::Acquire) as u64,
            total_length: self.total_length.load(AtomicOrdering::Acquire) as u64,
            vectors,
            doc_freq,
        };

        // Encode with bitcode
        let encoded = bitcode::encode(&data);

        // Build file: magic + version + data
        let mut file_data = Vec::with_capacity(4 + 2 + encoded.len() + 4);
        file_data.extend_from_slice(&SPARSE_INDEX_MAGIC);
        file_data.extend_from_slice(&SPARSE_INDEX_VERSION.to_le_bytes());
        file_data.extend_from_slice(&encoded);

        // Compute CRC32 checksum
        let mut hasher = Hasher::new();
        hasher.update(&file_data);
        let crc = hasher.finalize();
        file_data.extend_from_slice(&crc.to_le_bytes());

        // Atomic write: write to temp file then rename
        let temp_path = path.with_extension("tmp");
        let mut file = fs::File::create(&temp_path).map_err(|e| {
            Error::Vector(VectorError::IndexError(format!(
                "Failed to create temp file: {}",
                e
            )))
        })?;
        file.write_all(&file_data).map_err(|e| {
            Error::Vector(VectorError::IndexError(format!(
                "Failed to write sparse index: {}",
                e
            )))
        })?;
        file.sync_all().map_err(|e| {
            Error::Vector(VectorError::IndexError(format!(
                "Failed to sync sparse index: {}",
                e
            )))
        })?;
        drop(file);

        // Atomic rename
        fs::rename(&temp_path, path).map_err(|e| {
            Error::Vector(VectorError::IndexError(format!(
                "Failed to rename temp file: {}",
                e
            )))
        })?;

        Ok(())
    }

    /// Loads an index from a file.
    ///
    /// The config parameter is used for validation - the loaded index's
    /// dimensions must match the config's dimensions.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The file cannot be read
    /// - The magic bytes are invalid
    /// - The version is unsupported
    /// - The CRC32 checksum doesn't match
    /// - The dimensions don't match the config
    pub fn load(path: &Path, config: SparseIndexConfig) -> Result<Self> {
        let file_data = fs::read(path).map_err(|e| {
            Error::Vector(VectorError::IndexError(format!(
                "Failed to read sparse index file: {}",
                e
            )))
        })?;

        // Minimum size: magic(4) + version(2) + crc(4) = 10 bytes
        if file_data.len() < 10 {
            return Err(Error::Vector(VectorError::IndexError(
                "Sparse index file too small to be valid".to_string(),
            )));
        }

        // Verify magic bytes
        let magic: [u8; 4] = file_data[0..4].try_into().map_err(|_| {
            Error::Vector(VectorError::IndexError(
                "Failed to read magic bytes".to_string(),
            ))
        })?;
        if magic != SPARSE_INDEX_MAGIC {
            return Err(Error::Vector(VectorError::IndexError(format!(
                "Invalid magic bytes: expected {:?}, got {:?}",
                SPARSE_INDEX_MAGIC, magic
            ))));
        }

        // Check version
        let version = u16::from_le_bytes(file_data[4..6].try_into().map_err(|_| {
            Error::Vector(VectorError::IndexError(
                "Failed to read version".to_string(),
            ))
        })?);
        if version > SPARSE_INDEX_VERSION {
            return Err(Error::Vector(VectorError::IndexError(format!(
                "Unsupported sparse index version: {} (max supported: {})",
                version, SPARSE_INDEX_VERSION
            ))));
        }

        // Verify CRC32 checksum
        let crc_offset = file_data.len() - 4;
        let stored_crc = u32::from_le_bytes(file_data[crc_offset..].try_into().map_err(|_| {
            Error::Vector(VectorError::IndexError("Failed to read CRC32".to_string()))
        })?);

        let mut hasher = Hasher::new();
        hasher.update(&file_data[..crc_offset]);
        let computed_crc = hasher.finalize();

        if stored_crc != computed_crc {
            return Err(Error::Vector(VectorError::IndexError(format!(
                "CRC32 mismatch: stored={:#x}, computed={:#x}",
                stored_crc, computed_crc
            ))));
        }

        // Decode bitcode data (between version and CRC)
        let encoded_data = &file_data[6..crc_offset];
        let data: SparseIndexData = bitcode::decode(encoded_data).map_err(|e| {
            Error::Vector(VectorError::IndexError(format!(
                "Failed to decode sparse index: {}",
                e
            )))
        })?;

        // Validate dimensions match config
        if data.dimensions as usize != config.dimensions {
            return Err(Error::Vector(VectorError::DimensionMismatch {
                expected: config.dimensions,
                actual: data.dimensions as usize,
            }));
        }

        // Create the index with the loaded config
        let loaded_config = SparseIndexConfig {
            dimensions: data.dimensions as usize,
            scoring: data.scoring.into(),
            initial_capacity: data.count as usize,
        };

        let index = SparseVectorIndex {
            config: loaded_config,
            inverted_index: DashMap::with_capacity_and_hasher(
                data.count as usize,
                BuildIdentityHasher,
            ),
            vectors: DashMap::with_capacity_and_hasher(data.count as usize, BuildIdentityHasher),
            count: AtomicUsize::new(data.count as usize),
            total_length: AtomicUsize::new(data.total_length as usize),
            doc_freq: DashMap::with_capacity_and_hasher(data.doc_freq.len(), BuildIdentityHasher),
            write_lock: Mutex::new(()),
        };

        // Restore document frequencies
        for (dim, freq) in data.doc_freq {
            index.doc_freq.insert(dim, freq as usize);
        }

        // Restore vectors and rebuild inverted index
        for persisted in data.vectors {
            let node_id = NodeId::new(persisted.node_id).map_err(|_| {
                Error::Vector(VectorError::IndexError(format!(
                    "Invalid node ID: {}",
                    persisted.node_id
                )))
            })?;

            let vector = SparseVec::new(persisted.indices, persisted.values, data.dimensions)?;

            let magnitude = vector.magnitude();
            let stored = StoredVector {
                vector: Arc::new(vector),
                magnitude,
            };

            // Add to forward index
            index.vectors.insert(node_id, stored.clone());

            // Rebuild inverted index
            for (&dim, &val) in stored
                .vector
                .indices()
                .iter()
                .zip(stored.vector.values().iter())
            {
                index.inverted_index.entry(dim).or_default().push(Posting {
                    node_id,
                    value: val,
                });
            }
        }

        Ok(index)
    }

    /// Compacts the index by removing empty posting lists and shrinking capacity.
    ///
    /// This operation reclaims memory from removed vectors by:
    /// 1. Removing empty posting lists from the inverted index
    /// 2. Shrinking non-empty posting lists to fit their actual size
    /// 3. Removing zero-count entries from the document frequency map
    pub fn compact(&self) {
        // Acquire write lock to prevent concurrent modifications
        let _guard = self.write_lock.lock();

        // Pass 1: Remove empty posting lists
        self.inverted_index
            .retain(|_, postings| !postings.is_empty());

        // Pass 2: Shrink non-empty posting lists (separate pass to avoid
        // modifying entries during retain iteration)
        for mut entry in self.inverted_index.iter_mut() {
            entry.value_mut().shrink_to_fit();
        }

        // Remove zero-count document frequency entries
        self.doc_freq.retain(|_, &mut freq| freq > 0);
    }

    /// Returns statistics about the index.
    #[must_use]
    pub fn stats(&self) -> SparseIndexStats {
        let mut total_postings = 0;
        let mut non_empty_dimensions = 0;
        let mut max_posting_length = 0;

        for entry in self.inverted_index.iter() {
            let len = entry.value().len();
            if len > 0 {
                non_empty_dimensions += 1;
                total_postings += len;
                max_posting_length = max_posting_length.max(len);
            }
        }

        SparseIndexStats {
            num_vectors: self.len(),
            dimensions: self.config.dimensions,
            non_empty_dimensions,
            total_postings,
            avg_posting_length: if non_empty_dimensions > 0 {
                total_postings as f32 / non_empty_dimensions as f32
            } else {
                0.0
            },
            max_posting_length,
            avg_vector_nnz: if !self.is_empty() {
                self.total_length.load(AtomicOrdering::Acquire) as f32 / self.len() as f32
            } else {
                0.0
            },
            memory_usage: self.memory_usage(),
        }
    }
}

/// Statistics about a sparse vector index.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "config-toml", derive(serde::Serialize, serde::Deserialize))]
pub struct SparseIndexStats {
    /// Number of vectors in the index
    pub num_vectors: usize,
    /// Total dimensions
    pub dimensions: usize,
    /// Number of dimensions with at least one posting
    pub non_empty_dimensions: usize,
    /// Total number of postings across all dimensions
    pub total_postings: usize,
    /// Average posting list length
    pub avg_posting_length: f32,
    /// Maximum posting list length
    pub max_posting_length: usize,
    /// Average number of non-zero elements per vector
    pub avg_vector_nnz: f32,
    /// Approximate memory usage in bytes
    pub memory_usage: usize,
}

// ============================================================================
// Persistence Data Structures
// ============================================================================

/// Persisted scoring method (bitcode-serializable version).
#[derive(Debug, Clone, Encode, Decode)]
enum PersistedScoringMethod {
    DotProduct,
    Cosine,
    BM25 { k1: f32, b: f32 },
}

impl From<ScoringMethod> for PersistedScoringMethod {
    fn from(method: ScoringMethod) -> Self {
        match method {
            ScoringMethod::DotProduct => PersistedScoringMethod::DotProduct,
            ScoringMethod::Cosine => PersistedScoringMethod::Cosine,
            ScoringMethod::BM25 { k1, b } => PersistedScoringMethod::BM25 { k1, b },
        }
    }
}

impl From<PersistedScoringMethod> for ScoringMethod {
    fn from(method: PersistedScoringMethod) -> Self {
        match method {
            PersistedScoringMethod::DotProduct => ScoringMethod::DotProduct,
            PersistedScoringMethod::Cosine => ScoringMethod::Cosine,
            PersistedScoringMethod::BM25 { k1, b } => ScoringMethod::BM25 { k1, b },
        }
    }
}

/// Persisted sparse vector data.
#[derive(Debug, Clone, Encode, Decode)]
struct PersistedSparseVector {
    /// Node ID as u64
    node_id: u64,
    /// Sparse vector indices
    indices: Vec<u32>,
    /// Sparse vector values
    values: Vec<f32>,
}

/// Root data structure for sparse index persistence.
///
/// File format: `[magic:4][version:2][bitcode_data:N][crc32:4]`
#[derive(Debug, Clone, Encode, Decode)]
struct SparseIndexData {
    /// Magic bytes for validation (checked separately, not in bitcode)
    /// Format version (checked separately, not in bitcode)

    /// Vector dimensionality
    dimensions: u32,
    /// Scoring method
    scoring: PersistedScoringMethod,
    /// Number of vectors
    count: u64,
    /// Sum of all vector lengths (for BM25 avgdl)
    total_length: u64,
    /// All sparse vectors
    vectors: Vec<PersistedSparseVector>,
    /// Document frequency per dimension
    doc_freq: Vec<(u32, u64)>,
}

// ============================================================================
// Hybrid Search Support
// ============================================================================

/// Combines dense and sparse search results using score fusion.
///
/// This function merges results from a dense vector search (e.g., HNSW) and
/// a sparse vector search (inverted index) into a single ranked list.
///
/// # Arguments
///
/// * `dense_results` - Results from dense vector search (NodeId, similarity)
/// * `sparse_results` - Results from sparse vector search (NodeId, score)
/// * `alpha` - Weight for dense scores (0.0 to 1.0). Sparse weight is (1 - alpha).
/// * `k` - Maximum number of results to return
///
/// # Returns
///
/// Combined and re-ranked results as (NodeId, fused_score) pairs.
///
/// # Score Normalization
///
/// Both dense and sparse scores are min-max normalized to [0, 1] before fusion.
/// This ensures fair combination regardless of the original score ranges.
///
/// # Example
///
/// ```rust,no_run
/// use aletheiadb::index::vector::sparse::hybrid_fusion;
/// use aletheiadb::core::id::NodeId;
///
/// let dense_results = vec![
///     (NodeId::new(1).unwrap(), 0.95),
///     (NodeId::new(2).unwrap(), 0.85),
/// ];
/// let sparse_results = vec![
///     (NodeId::new(2).unwrap(), 12.5),
///     (NodeId::new(3).unwrap(), 10.0),
/// ];
///
/// // 70% weight to dense, 30% to sparse
/// let fused = hybrid_fusion(&dense_results, &sparse_results, 0.7, 10);
/// ```
pub fn hybrid_fusion(
    dense_results: &[(NodeId, f32)],
    sparse_results: &[(NodeId, f32)],
    alpha: f32,
    k: usize,
) -> Vec<(NodeId, f32)> {
    let alpha = alpha.clamp(0.0, 1.0);
    let k = k.min(MAX_K);

    if dense_results.is_empty() && sparse_results.is_empty() {
        return Vec::new();
    }

    // Normalize dense scores to [0, 1]
    let dense_normalized = normalize_scores(dense_results);

    // Normalize sparse scores to [0, 1]
    let sparse_normalized = normalize_scores(sparse_results);

    // Combine scores
    let mut combined: HashMap<NodeId, f32, BuildIdentityHasher> =
        HashMap::with_hasher(BuildIdentityHasher);

    for (id, score) in dense_normalized {
        *combined.entry(id).or_insert(0.0) += alpha * score;
    }

    for (id, score) in sparse_normalized {
        *combined.entry(id).or_insert(0.0) += (1.0 - alpha) * score;
    }

    // Sort by combined score and take top k
    let mut results: Vec<(NodeId, f32)> = combined.into_iter().collect();
    // Use total_cmp for consistent ordering that handles NaN
    results.sort_by(|a, b| b.1.total_cmp(&a.1));
    results.truncate(k);

    results
}

/// Reciprocal Rank Fusion for combining ranked lists.
///
/// RRF is a robust fusion method that only uses rank positions, not scores.
/// This makes it more robust to score distribution differences.
///
/// # Arguments
///
/// * `dense_results` - Results from dense vector search
/// * `sparse_results` - Results from sparse vector search
/// * `k_constant` - RRF smoothing constant (typical: 60)
/// * `k` - Maximum number of results to return
///
/// # Formula
///
/// RRF score = sum(1 / (k_constant + rank))
///
/// # Example
///
/// ```rust,no_run
/// use aletheiadb::index::vector::sparse::reciprocal_rank_fusion;
/// use aletheiadb::core::id::NodeId;
///
/// let dense_results = vec![
///     (NodeId::new(1).unwrap(), 0.95),
///     (NodeId::new(2).unwrap(), 0.85),
/// ];
/// let sparse_results = vec![
///     (NodeId::new(2).unwrap(), 12.5),
///     (NodeId::new(3).unwrap(), 10.0),
/// ];
///
/// let fused = reciprocal_rank_fusion(&dense_results, &sparse_results, 60.0, 10);
/// ```
pub fn reciprocal_rank_fusion(
    dense_results: &[(NodeId, f32)],
    sparse_results: &[(NodeId, f32)],
    k_constant: f32,
    k: usize,
) -> Vec<(NodeId, f32)> {
    let k = k.min(MAX_K);
    let k_constant = k_constant.max(1.0);

    let mut rrf_scores: HashMap<NodeId, f32, BuildIdentityHasher> =
        HashMap::with_hasher(BuildIdentityHasher);

    // Add RRF contribution from dense results
    for (rank, (id, _)) in dense_results.iter().enumerate() {
        *rrf_scores.entry(*id).or_insert(0.0) += 1.0 / (k_constant + rank as f32 + 1.0);
    }

    // Add RRF contribution from sparse results
    for (rank, (id, _)) in sparse_results.iter().enumerate() {
        *rrf_scores.entry(*id).or_insert(0.0) += 1.0 / (k_constant + rank as f32 + 1.0);
    }

    // Sort by RRF score and take top k
    let mut results: Vec<(NodeId, f32)> = rrf_scores.into_iter().collect();
    // Use total_cmp for consistent ordering that handles NaN
    results.sort_by(|a, b| b.1.total_cmp(&a.1));
    results.truncate(k);

    results
}

/// Normalizes scores to [0, 1] range using min-max normalization.
fn normalize_scores(results: &[(NodeId, f32)]) -> Vec<(NodeId, f32)> {
    if results.is_empty() {
        return Vec::new();
    }

    let min_score = results
        .iter()
        .map(|(_, s)| *s)
        .fold(f32::INFINITY, f32::min);
    let max_score = results
        .iter()
        .map(|(_, s)| *s)
        .fold(f32::NEG_INFINITY, f32::max);
    let range = max_score - min_score;

    if range == 0.0 {
        // All scores are the same, normalize to 1.0
        return results.iter().map(|(id, _)| (*id, 1.0)).collect();
    }

    results
        .iter()
        .map(|(id, score)| (*id, (score - min_score) / range))
        .collect()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    // ========================================================================
    // Basic Functionality Tests
    // ========================================================================

    #[test]
    fn test_create_sparse_index() {
        let config = SparseIndexConfig::new(10_000);
        let index = SparseVectorIndex::new(config).unwrap();

        assert_eq!(index.dimensions(), 10_000);
        assert_eq!(index.len(), 0);
        assert!(index.is_empty());
    }

    #[test]
    fn test_create_sparse_index_zero_dimensions_fails() {
        let config = SparseIndexConfig::new(0);
        let result = SparseVectorIndex::new(config);

        assert!(result.is_err());
    }

    #[test]
    fn test_add_and_retrieve_vector() {
        let index = SparseVectorIndex::new(SparseIndexConfig::new(100)).unwrap();
        let node_id = NodeId::new(1).unwrap();
        let vector = SparseVec::new(vec![10, 50, 90], vec![1.0, 2.0, 3.0], 100).unwrap();

        index.add(node_id, &vector).unwrap();

        assert_eq!(index.len(), 1);
        assert!(!index.is_empty());
        assert!(index.contains(node_id));

        let retrieved = index.get(node_id).unwrap();
        assert_eq!(retrieved.dimension(), 100);
        assert_eq!(retrieved.nnz(), 3);
    }

    #[test]
    fn test_add_dimension_mismatch() {
        let index = SparseVectorIndex::new(SparseIndexConfig::new(100)).unwrap();
        let node_id = NodeId::new(1).unwrap();
        let vector = SparseVec::new(vec![10], vec![1.0], 200).unwrap(); // Wrong dimension

        let result = index.add(node_id, &vector);

        assert!(matches!(
            result,
            Err(Error::Vector(VectorError::DimensionMismatch { .. }))
        ));
    }

    #[test]
    fn test_remove_vector() {
        let index = SparseVectorIndex::new(SparseIndexConfig::new(100)).unwrap();
        let node_id = NodeId::new(1).unwrap();
        let vector = SparseVec::new(vec![10, 50], vec![1.0, 2.0], 100).unwrap();

        index.add(node_id, &vector).unwrap();
        assert_eq!(index.len(), 1);

        index.remove(node_id).unwrap();
        assert_eq!(index.len(), 0);
        assert!(!index.contains(node_id));
    }

    #[test]
    fn test_remove_nonexistent_vector() {
        let index = SparseVectorIndex::new(SparseIndexConfig::new(100)).unwrap();
        let node_id = NodeId::new(999).unwrap();

        // Should not error
        index.remove(node_id).unwrap();
        assert_eq!(index.len(), 0);
    }

    #[test]
    fn test_update_existing_vector() {
        let index = SparseVectorIndex::new(SparseIndexConfig::new(100)).unwrap();
        let node_id = NodeId::new(1).unwrap();

        let vector1 = SparseVec::new(vec![10], vec![1.0], 100).unwrap();
        index.add(node_id, &vector1).unwrap();
        assert_eq!(index.len(), 1);

        // Add again should replace
        let vector2 = SparseVec::new(vec![20, 30], vec![2.0, 3.0], 100).unwrap();
        index.add(node_id, &vector2).unwrap();
        assert_eq!(index.len(), 1);

        let retrieved = index.get(node_id).unwrap();
        assert_eq!(retrieved.nnz(), 2);
        assert_eq!(retrieved.indices(), &[20, 30]);
    }

    // ========================================================================
    // Search Tests - Dot Product
    // ========================================================================

    #[test]
    fn test_search_dot_product_basic() {
        let config = SparseIndexConfig::new(100).with_scoring(ScoringMethod::DotProduct);
        let index = SparseVectorIndex::new(config).unwrap();

        // Add two documents
        let doc1 = SparseVec::new(vec![0, 5, 10], vec![1.0, 2.0, 3.0], 100).unwrap();
        let doc2 = SparseVec::new(vec![5, 10, 15], vec![1.0, 1.0, 1.0], 100).unwrap();

        index.add(NodeId::new(1).unwrap(), &doc1).unwrap();
        index.add(NodeId::new(2).unwrap(), &doc2).unwrap();

        // Query overlaps with both docs at dimensions 5 and 10
        let query = SparseVec::new(vec![5, 10], vec![1.0, 1.0], 100).unwrap();
        let results = index.search(&query, 10).unwrap();

        assert_eq!(results.len(), 2);

        // doc1: 2.0*1.0 + 3.0*1.0 = 5.0
        // doc2: 1.0*1.0 + 1.0*1.0 = 2.0
        // doc1 should score higher
        assert_eq!(results[0].0, NodeId::new(1).unwrap());
        assert!((results[0].1 - 5.0).abs() < 1e-6);
        assert_eq!(results[1].0, NodeId::new(2).unwrap());
        assert!((results[1].1 - 2.0).abs() < 1e-6);
    }

    #[test]
    fn test_search_no_overlap() {
        let config = SparseIndexConfig::new(100);
        let index = SparseVectorIndex::new(config).unwrap();

        let doc = SparseVec::new(vec![0, 1, 2], vec![1.0, 2.0, 3.0], 100).unwrap();
        index.add(NodeId::new(1).unwrap(), &doc).unwrap();

        // Query has no overlapping dimensions
        let query = SparseVec::new(vec![50, 60], vec![1.0, 1.0], 100).unwrap();
        let results = index.search(&query, 10).unwrap();

        // No results since there's no overlap
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_empty_index() {
        let index = SparseVectorIndex::new(SparseIndexConfig::new(100)).unwrap();
        let query = SparseVec::new(vec![0], vec![1.0], 100).unwrap();

        let results = index.search(&query, 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_k_zero() {
        let index = SparseVectorIndex::new(SparseIndexConfig::new(100)).unwrap();
        let doc = SparseVec::new(vec![0], vec![1.0], 100).unwrap();
        index.add(NodeId::new(1).unwrap(), &doc).unwrap();

        let query = SparseVec::new(vec![0], vec![1.0], 100).unwrap();
        let results = index.search(&query, 0).unwrap();

        assert!(results.is_empty());
    }

    #[test]
    fn test_search_top_k() {
        let index = SparseVectorIndex::new(SparseIndexConfig::new(100)).unwrap();

        // Add 10 documents
        for i in 1..=10 {
            let doc = SparseVec::new(vec![0], vec![i as f32], 100).unwrap();
            index.add(NodeId::new(i).unwrap(), &doc).unwrap();
        }

        let query = SparseVec::new(vec![0], vec![1.0], 100).unwrap();
        let results = index.search(&query, 3).unwrap();

        assert_eq!(results.len(), 3);
        // Should get top 3 by score (doc 10, 9, 8)
        assert_eq!(results[0].0, NodeId::new(10).unwrap());
        assert_eq!(results[1].0, NodeId::new(9).unwrap());
        assert_eq!(results[2].0, NodeId::new(8).unwrap());
    }

    // ========================================================================
    // Search Tests - Cosine Similarity
    // ========================================================================

    #[test]
    fn test_search_cosine_similarity() {
        let config = SparseIndexConfig::new(100).with_scoring(ScoringMethod::Cosine);
        let index = SparseVectorIndex::new(config).unwrap();

        // Add identical vectors with different magnitudes
        let doc1 = SparseVec::new(vec![0, 1], vec![1.0, 1.0], 100).unwrap();
        let doc2 = SparseVec::new(vec![0, 1], vec![10.0, 10.0], 100).unwrap();

        index.add(NodeId::new(1).unwrap(), &doc1).unwrap();
        index.add(NodeId::new(2).unwrap(), &doc2).unwrap();

        let query = SparseVec::new(vec![0, 1], vec![1.0, 1.0], 100).unwrap();
        let results = index.search(&query, 10).unwrap();

        // Both should have same cosine similarity (1.0) since they're parallel
        assert_eq!(results.len(), 2);
        assert!((results[0].1 - 1.0).abs() < 1e-5);
        assert!((results[1].1 - 1.0).abs() < 1e-5);
    }

    #[test]
    fn test_search_cosine_orthogonal() {
        let config = SparseIndexConfig::new(100).with_scoring(ScoringMethod::Cosine);
        let index = SparseVectorIndex::new(config).unwrap();

        // Orthogonal vectors (no overlap)
        let doc = SparseVec::new(vec![0, 1], vec![1.0, 1.0], 100).unwrap();
        index.add(NodeId::new(1).unwrap(), &doc).unwrap();

        let query = SparseVec::new(vec![50, 51], vec![1.0, 1.0], 100).unwrap();
        let results = index.search(&query, 10).unwrap();

        // Orthogonal vectors should have 0 similarity
        assert!(results.is_empty());
    }

    // ========================================================================
    // Search Tests - BM25
    // ========================================================================

    #[test]
    fn test_search_bm25() {
        let config = SparseIndexConfig::new(100).with_scoring(ScoringMethod::bm25_default());
        let index = SparseVectorIndex::new(config).unwrap();

        // Add documents with different term frequencies
        let doc1 = SparseVec::new(vec![0, 1, 2], vec![3.0, 1.0, 1.0], 100).unwrap(); // term 0 appears 3 times
        let doc2 = SparseVec::new(vec![0, 3, 4], vec![1.0, 1.0, 1.0], 100).unwrap(); // term 0 appears 1 time

        index.add(NodeId::new(1).unwrap(), &doc1).unwrap();
        index.add(NodeId::new(2).unwrap(), &doc2).unwrap();

        // Query for term 0
        let query = SparseVec::new(vec![0], vec![1.0], 100).unwrap();
        let results = index.search(&query, 10).unwrap();

        assert_eq!(results.len(), 2);
        // BM25 should rank doc1 higher due to higher term frequency
        // (with saturation, so not proportionally higher)
        assert!(results[0].1 > results[1].1);
    }

    // ========================================================================
    // Search with Filter Tests
    // ========================================================================

    #[test]
    fn test_search_with_filter() {
        let index = SparseVectorIndex::new(SparseIndexConfig::new(100)).unwrap();

        for i in 1..=10 {
            let doc = SparseVec::new(vec![0], vec![i as f32], 100).unwrap();
            index.add(NodeId::new(i).unwrap(), &doc).unwrap();
        }

        let query = SparseVec::new(vec![0], vec![1.0], 100).unwrap();

        // Only allow even node IDs
        let allowed: HashSet<NodeId> = (1..=10)
            .filter(|i| i % 2 == 0)
            .map(|i| NodeId::new(i).unwrap())
            .collect();

        let results = index
            .search_with_filter(&query, 10, |id| allowed.contains(id))
            .unwrap();

        assert_eq!(results.len(), 5);
        for (id, _) in &results {
            assert!(allowed.contains(id));
        }
    }

    // ========================================================================
    // Statistics Tests
    // ========================================================================

    #[test]
    fn test_index_stats() {
        let index = SparseVectorIndex::new(SparseIndexConfig::new(1000)).unwrap();

        // Add some vectors
        let doc1 = SparseVec::new(vec![0, 100, 500], vec![1.0, 2.0, 3.0], 1000).unwrap();
        let doc2 = SparseVec::new(vec![0, 200], vec![1.0, 1.0], 1000).unwrap();
        let doc3 = SparseVec::new(vec![0, 100, 200, 300], vec![1.0, 1.0, 1.0, 1.0], 1000).unwrap();

        index.add(NodeId::new(1).unwrap(), &doc1).unwrap();
        index.add(NodeId::new(2).unwrap(), &doc2).unwrap();
        index.add(NodeId::new(3).unwrap(), &doc3).unwrap();

        let stats = index.stats();

        assert_eq!(stats.num_vectors, 3);
        assert_eq!(stats.dimensions, 1000);
        assert!(stats.non_empty_dimensions > 0);
        assert_eq!(stats.total_postings, 9); // 3 + 2 + 4 = 9 postings
        assert!(stats.avg_vector_nnz > 0.0);
    }

    // ========================================================================
    // Hybrid Fusion Tests
    // ========================================================================

    #[test]
    fn test_hybrid_fusion_basic() {
        let dense = vec![
            (NodeId::new(1).unwrap(), 0.9),
            (NodeId::new(2).unwrap(), 0.85),
            (NodeId::new(4).unwrap(), 0.7),
        ];
        let sparse = vec![
            (NodeId::new(2).unwrap(), 10.0),
            (NodeId::new(3).unwrap(), 8.0),
            (NodeId::new(4).unwrap(), 6.0),
        ];

        // Equal weight (0.5)
        let fused = hybrid_fusion(&dense, &sparse, 0.5, 10);

        // All four nodes should be present
        assert_eq!(fused.len(), 4);

        // Node 2 appears in both with high scores, should have highest fused score
        // Dense normalized: 1→1.0, 2→0.75, 4→0.0 (range: 0.7-0.9)
        // Sparse normalized: 2→1.0, 3→0.5, 4→0.0 (range: 6.0-10.0)
        // Node 2: 0.5*0.75 + 0.5*1.0 = 0.875 (highest)
        // Node 1: 0.5*1.0 + 0.0 = 0.5
        // Node 3: 0.0 + 0.5*0.5 = 0.25
        // Node 4: 0.5*0.0 + 0.5*0.0 = 0.0
        assert_eq!(fused[0].0, NodeId::new(2).unwrap());
    }

    #[test]
    fn test_hybrid_fusion_dense_only() {
        let dense = vec![
            (NodeId::new(1).unwrap(), 0.9),
            (NodeId::new(2).unwrap(), 0.8),
        ];
        let sparse: Vec<(NodeId, f32)> = vec![];

        let fused = hybrid_fusion(&dense, &sparse, 0.5, 10);

        assert_eq!(fused.len(), 2);
        // Order should be preserved
        assert_eq!(fused[0].0, NodeId::new(1).unwrap());
    }

    #[test]
    fn test_hybrid_fusion_sparse_only() {
        let dense: Vec<(NodeId, f32)> = vec![];
        let sparse = vec![
            (NodeId::new(1).unwrap(), 10.0),
            (NodeId::new(2).unwrap(), 8.0),
        ];

        let fused = hybrid_fusion(&dense, &sparse, 0.5, 10);

        assert_eq!(fused.len(), 2);
        assert_eq!(fused[0].0, NodeId::new(1).unwrap());
    }

    #[test]
    fn test_hybrid_fusion_alpha_extremes() {
        let dense = vec![(NodeId::new(1).unwrap(), 0.9)];
        let sparse = vec![(NodeId::new(2).unwrap(), 10.0)];

        // Alpha = 1.0 (dense only)
        let fused = hybrid_fusion(&dense, &sparse, 1.0, 10);
        assert_eq!(fused[0].0, NodeId::new(1).unwrap());
        assert!(fused[0].1 > fused[1].1); // Dense result should be higher

        // Alpha = 0.0 (sparse only)
        let fused = hybrid_fusion(&dense, &sparse, 0.0, 10);
        assert_eq!(fused[0].0, NodeId::new(2).unwrap());
    }

    #[test]
    fn test_reciprocal_rank_fusion() {
        let dense = vec![
            (NodeId::new(1).unwrap(), 0.9),
            (NodeId::new(2).unwrap(), 0.8),
            (NodeId::new(3).unwrap(), 0.7),
        ];
        let sparse = vec![
            (NodeId::new(2).unwrap(), 10.0),
            (NodeId::new(4).unwrap(), 8.0),
            (NodeId::new(1).unwrap(), 6.0),
        ];

        let fused = reciprocal_rank_fusion(&dense, &sparse, 60.0, 10);

        // Node 1 and 2 appear in both lists, should be ranked high
        assert!(fused.len() <= 4);

        // Node 2 is rank 2 in dense (1/(60+2)) and rank 1 in sparse (1/(60+1))
        // Node 1 is rank 1 in dense (1/(60+1)) and rank 3 in sparse (1/(60+3))
        // They should both have high RRF scores
    }

    // ========================================================================
    // Thread Safety Tests
    // ========================================================================

    #[test]
    fn test_concurrent_adds() {
        use std::thread;

        let index = Arc::new(SparseVectorIndex::new(SparseIndexConfig::new(100)).unwrap());
        let mut handles = vec![];

        for i in 0..10 {
            let index_clone = Arc::clone(&index);
            let handle = thread::spawn(move || {
                let doc = SparseVec::new(vec![i as u32], vec![1.0], 100).unwrap();
                index_clone.add(NodeId::new(i + 1).unwrap(), &doc).unwrap();
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.join().unwrap();
        }

        assert_eq!(index.len(), 10);
    }

    #[test]
    fn test_concurrent_search() {
        use std::thread;

        let index = Arc::new(SparseVectorIndex::new(SparseIndexConfig::new(200)).unwrap());

        // Add some documents with dimension 0 (shared) and a unique dimension
        for i in 1..=100 {
            // Use dimension 0 as shared, and i as unique (values 1-100, all valid for dim 200)
            let doc = SparseVec::new(vec![0, i as u32], vec![1.0, i as f32], 200).unwrap();
            index.add(NodeId::new(i).unwrap(), &doc).unwrap();
        }

        let mut handles = vec![];

        // Concurrent searches
        for _ in 0..10 {
            let index_clone = Arc::clone(&index);
            let handle = thread::spawn(move || {
                let query = SparseVec::new(vec![0], vec![1.0], 200).unwrap();
                let results = index_clone.search(&query, 10).unwrap();
                assert_eq!(results.len(), 10);
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.join().unwrap();
        }
    }

    // ========================================================================
    // Edge Case Tests
    // ========================================================================

    #[test]
    fn test_empty_sparse_vector() {
        let index = SparseVectorIndex::new(SparseIndexConfig::new(100)).unwrap();
        let empty = SparseVec::new(vec![], vec![], 100).unwrap();

        index.add(NodeId::new(1).unwrap(), &empty).unwrap();
        assert_eq!(index.len(), 1);

        // Empty query should return nothing
        let query = SparseVec::new(vec![], vec![], 100).unwrap();
        let results = index.search(&query, 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_single_dimension_vectors() {
        let index = SparseVectorIndex::new(SparseIndexConfig::new(10_000)).unwrap();

        // All vectors have only dimension 0
        for i in 1..=100 {
            let doc = SparseVec::new(vec![0], vec![i as f32], 10_000).unwrap();
            index.add(NodeId::new(i).unwrap(), &doc).unwrap();
        }

        let query = SparseVec::new(vec![0], vec![1.0], 10_000).unwrap();
        let results = index.search(&query, 5).unwrap();

        assert_eq!(results.len(), 5);
        // Should return top 5 by score
        assert_eq!(results[0].0, NodeId::new(100).unwrap());
    }

    #[test]
    fn test_very_sparse_high_dimensional() {
        let dim = 100_000;
        let index = SparseVectorIndex::new(SparseIndexConfig::new(dim)).unwrap();

        // Very sparse: 3 non-zeros in 100,000 dimensions
        let doc = SparseVec::new(vec![0, 50_000, 99_999], vec![1.0, 2.0, 3.0], dim as u32).unwrap();
        index.add(NodeId::new(1).unwrap(), &doc).unwrap();

        let query = SparseVec::new(vec![50_000], vec![1.0], dim as u32).unwrap();
        let results = index.search(&query, 10).unwrap();

        assert_eq!(results.len(), 1);
        assert!((results[0].1 - 2.0).abs() < 1e-6);
    }

    #[test]
    fn test_compact() {
        let index = SparseVectorIndex::new(SparseIndexConfig::new(100)).unwrap();

        // Add and remove vectors
        for i in 1..=10 {
            let doc = SparseVec::new(vec![i as u32], vec![1.0], 100).unwrap();
            index.add(NodeId::new(i).unwrap(), &doc).unwrap();
        }

        for i in 1..=10 {
            index.remove(NodeId::new(i).unwrap()).unwrap();
        }

        assert_eq!(index.len(), 0);

        // Compact should clean up empty posting lists
        index.compact();

        let stats = index.stats();
        assert_eq!(stats.total_postings, 0);
    }

    #[test]
    fn test_memory_usage() {
        let index = SparseVectorIndex::new(SparseIndexConfig::new(1000)).unwrap();

        let initial_mem = index.memory_usage();

        // Add vectors
        for i in 1..=100 {
            let doc = SparseVec::new(vec![0, 1, 2], vec![1.0, 2.0, 3.0], 1000).unwrap();
            index.add(NodeId::new(i).unwrap(), &doc).unwrap();
        }

        let final_mem = index.memory_usage();
        assert!(final_mem > initial_mem);
    }

    // ========================================================================
    // Configuration Tests
    // ========================================================================

    #[test]
    fn test_config_builder() {
        let config = SparseIndexConfig::new(1000)
            .with_scoring(ScoringMethod::Cosine)
            .with_capacity(5000);

        assert_eq!(config.dimensions, 1000);
        assert_eq!(config.scoring, ScoringMethod::Cosine);
        assert_eq!(config.initial_capacity, 5000);
    }

    #[test]
    fn test_bm25_custom_params() {
        let scoring = ScoringMethod::BM25 { k1: 2.0, b: 0.5 };
        let config = SparseIndexConfig::new(100).with_scoring(scoring);

        if let ScoringMethod::BM25 { k1, b } = config.scoring {
            assert_eq!(k1, 2.0);
            assert_eq!(b, 0.5);
        } else {
            panic!("Expected BM25 scoring");
        }
    }

    // ========================================================================
    // Edge Case Tests
    // ========================================================================

    #[test]
    fn test_max_dimensions_boundary() {
        // Exactly at MAX_VECTOR_DIMENSIONS should succeed
        let result = SparseVectorIndex::new(SparseIndexConfig::new(MAX_VECTOR_DIMENSIONS));
        assert!(result.is_ok());

        // One over MAX_VECTOR_DIMENSIONS should fail
        let result = SparseVectorIndex::new(SparseIndexConfig::new(MAX_VECTOR_DIMENSIONS + 1));
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::DimensionTooLarge {
                dimension,
                max_allowed,
            })) => {
                assert_eq!(dimension, MAX_VECTOR_DIMENSIONS + 1);
                assert_eq!(max_allowed, MAX_VECTOR_DIMENSIONS);
            }
            _ => panic!("Expected DimensionTooLarge error"),
        }
    }

    #[test]
    fn test_max_k_capping() {
        let index = SparseVectorIndex::new(SparseIndexConfig::new(100)).unwrap();

        // Add more vectors than MAX_K
        for i in 1..=100 {
            let doc = SparseVec::new(vec![0], vec![i as f32], 100).unwrap();
            index.add(NodeId::new(i).unwrap(), &doc).unwrap();
        }

        // Request more than MAX_K (10_000), should be capped
        let query = SparseVec::new(vec![0], vec![1.0], 100).unwrap();
        let results = index.search(&query, 100_000).unwrap();

        // Should return at most 100 (since we only have 100 vectors)
        // but the request was capped to MAX_K internally
        assert!(results.len() <= 100);
    }

    #[test]
    fn test_nan_values_in_search_results() {
        let index = SparseVectorIndex::new(SparseIndexConfig::new(100)).unwrap();

        // Add some normal vectors
        for i in 1..=5 {
            let doc = SparseVec::new(vec![0], vec![i as f32], 100).unwrap();
            index.add(NodeId::new(i).unwrap(), &doc).unwrap();
        }

        let query = SparseVec::new(vec![0], vec![1.0], 100).unwrap();
        let results = index.search(&query, 10).unwrap();

        // Results should be properly sorted without panicking
        // (tests that total_cmp handles NaN correctly if any arise)
        assert!(!results.is_empty());

        // Verify results are sorted by score descending
        for i in 1..results.len() {
            assert!(
                results[i - 1].1 >= results[i].1 || results[i].1.is_nan(),
                "Results should be sorted by score descending"
            );
        }
    }

    #[test]
    fn test_concurrent_add_remove_same_node() {
        use std::sync::Arc;
        use std::thread;

        let index = Arc::new(SparseVectorIndex::new(SparseIndexConfig::new(100)).unwrap());
        let num_threads = 4;
        let iterations = 100;

        let handles: Vec<_> = (0..num_threads)
            .map(|thread_id| {
                let index = Arc::clone(&index);
                thread::spawn(move || {
                    for i in 0..iterations {
                        let node_id = NodeId::new(1).unwrap(); // All threads use same node ID
                        let doc = SparseVec::new(
                            vec![(thread_id * iterations + i) as u32 % 50],
                            vec![1.0],
                            100,
                        )
                        .unwrap();

                        // Add and remove the same node concurrently
                        let _ = index.add(node_id, &doc);
                        let _ = index.remove(node_id);
                    }
                })
            })
            .collect();

        for handle in handles {
            handle.join().unwrap();
        }

        // After all operations, index should be in a consistent state
        // (may have 0 or 1 vectors depending on timing)
        assert!(index.len() <= 1);

        // Search should work without errors
        let query = SparseVec::new(vec![0], vec![1.0], 100).unwrap();
        let results = index.search(&query, 10);
        assert!(results.is_ok());
    }

    // ========================================================================
    // Persistence Tests
    // ========================================================================

    #[test]
    fn test_save_and_load_basic() {
        use std::fs;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let path = dir.path().join("sparse_index.gsp");

        // Create index with some data
        let config = SparseIndexConfig::new(100).with_scoring(ScoringMethod::DotProduct);
        let index = SparseVectorIndex::new(config.clone()).unwrap();

        let v1 = SparseVec::new(vec![0, 10, 50], vec![1.0, 2.0, 3.0], 100).unwrap();
        let v2 = SparseVec::new(vec![10, 20, 30], vec![0.5, 1.5, 2.5], 100).unwrap();

        index.add(NodeId::new(1).unwrap(), &v1).unwrap();
        index.add(NodeId::new(2).unwrap(), &v2).unwrap();

        // Save
        index.save(&path).unwrap();
        assert!(path.exists());

        // Load
        let loaded = SparseVectorIndex::load(&path, config).unwrap();

        // Verify
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded.dimensions(), 100);

        // Check vectors are intact
        let loaded_v1 = loaded.get(NodeId::new(1).unwrap()).unwrap();
        assert_eq!(loaded_v1.indices(), v1.indices());
        assert_eq!(loaded_v1.values(), v1.values());

        let loaded_v2 = loaded.get(NodeId::new(2).unwrap()).unwrap();
        assert_eq!(loaded_v2.indices(), v2.indices());
        assert_eq!(loaded_v2.values(), v2.values());

        // Search should work on loaded index
        let query = SparseVec::new(vec![10], vec![1.0], 100).unwrap();
        let results = loaded.search(&query, 10).unwrap();
        assert_eq!(results.len(), 2);

        // Clean up
        fs::remove_file(&path).ok();
    }

    #[test]
    fn test_save_and_load_bm25() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let path = dir.path().join("sparse_bm25.gsp");

        // Create index with BM25 scoring
        let config =
            SparseIndexConfig::new(1000).with_scoring(ScoringMethod::BM25 { k1: 1.8, b: 0.6 });
        let index = SparseVectorIndex::new(config.clone()).unwrap();

        for i in 1..=10 {
            let v = SparseVec::new(vec![i as u32, (i * 10) as u32], vec![1.0, 2.0], 1000).unwrap();
            index.add(NodeId::new(i).unwrap(), &v).unwrap();
        }

        // Save and load
        index.save(&path).unwrap();
        let loaded = SparseVectorIndex::load(&path, config).unwrap();

        // Verify BM25 parameters preserved
        if let ScoringMethod::BM25 { k1, b } = loaded.scoring() {
            assert!((k1 - 1.8).abs() < 1e-6);
            assert!((b - 0.6).abs() < 1e-6);
        } else {
            panic!("Expected BM25 scoring method");
        }

        assert_eq!(loaded.len(), 10);
    }

    #[test]
    fn test_save_and_load_cosine() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let path = dir.path().join("sparse_cosine.gsp");

        let config = SparseIndexConfig::new(500).with_scoring(ScoringMethod::Cosine);
        let index = SparseVectorIndex::new(config.clone()).unwrap();

        let v = SparseVec::new(vec![0, 100, 200], vec![1.0, 1.0, 1.0], 500).unwrap();
        index.add(NodeId::new(42).unwrap(), &v).unwrap();

        index.save(&path).unwrap();
        let loaded = SparseVectorIndex::load(&path, config).unwrap();

        assert_eq!(loaded.scoring(), ScoringMethod::Cosine);
        assert_eq!(loaded.len(), 1);
        assert!(loaded.contains(NodeId::new(42).unwrap()));
    }

    #[test]
    fn test_save_and_load_empty_index() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let path = dir.path().join("sparse_empty.gsp");

        let config = SparseIndexConfig::new(100);
        let index = SparseVectorIndex::new(config.clone()).unwrap();

        // Save empty index
        index.save(&path).unwrap();

        // Load empty index
        let loaded = SparseVectorIndex::load(&path, config).unwrap();
        assert_eq!(loaded.len(), 0);
        assert!(loaded.is_empty());
    }

    #[test]
    fn test_load_invalid_magic() {
        use std::fs;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let path = dir.path().join("invalid_magic.gsp");

        // Write file with invalid magic
        fs::write(&path, b"BADM\x01\x00\x00\x00\x00\x00").unwrap();

        let config = SparseIndexConfig::new(100);
        let result = SparseVectorIndex::load(&path, config);
        assert!(result.is_err());
        let err = format!("{}", result.err().unwrap());
        assert!(err.contains("Invalid magic bytes"));
    }

    #[test]
    fn test_load_corrupted_crc() {
        use std::fs;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let path = dir.path().join("corrupted.gsp");

        // Create valid index
        let config = SparseIndexConfig::new(100);
        let index = SparseVectorIndex::new(config.clone()).unwrap();
        let v = SparseVec::new(vec![0], vec![1.0], 100).unwrap();
        index.add(NodeId::new(1).unwrap(), &v).unwrap();
        index.save(&path).unwrap();

        // Corrupt the last byte (part of CRC)
        let mut data = fs::read(&path).unwrap();
        let last_idx = data.len() - 1;
        data[last_idx] ^= 0xFF;
        fs::write(&path, &data).unwrap();

        // Load should fail
        let result = SparseVectorIndex::load(&path, config);
        assert!(result.is_err());
        let err = format!("{}", result.err().unwrap());
        assert!(err.contains("CRC32 mismatch"));
    }

    #[test]
    fn test_load_dimension_mismatch() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let path = dir.path().join("dim_mismatch.gsp");

        // Save with 100 dimensions
        let config100 = SparseIndexConfig::new(100);
        let index = SparseVectorIndex::new(config100).unwrap();
        index.save(&path).unwrap();

        // Try to load with 200 dimensions
        let config200 = SparseIndexConfig::new(200);
        let result = SparseVectorIndex::load(&path, config200);
        assert!(result.is_err());
    }

    #[test]
    fn test_load_file_too_small() {
        use std::fs;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let path = dir.path().join("too_small.gsp");

        // Write file that's too small
        fs::write(&path, b"GSPS").unwrap();

        let config = SparseIndexConfig::new(100);
        let result = SparseVectorIndex::load(&path, config);
        assert!(result.is_err());
        let err = format!("{}", result.err().unwrap());
        assert!(err.contains("too small"));
    }

    #[test]
    fn test_save_and_load_preserves_search_results() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let path = dir.path().join("search_preserve.gsp");

        let config = SparseIndexConfig::new(100);
        let index = SparseVectorIndex::new(config.clone()).unwrap();

        // Add documents with varying relevance
        for i in 1..=5 {
            let v = SparseVec::new(vec![0, 1], vec![i as f32, (6 - i) as f32], 100).unwrap();
            index.add(NodeId::new(i).unwrap(), &v).unwrap();
        }

        let query = SparseVec::new(vec![0], vec![1.0], 100).unwrap();
        let results_before = index.search(&query, 5).unwrap();

        // Save and load
        index.save(&path).unwrap();
        let loaded = SparseVectorIndex::load(&path, config).unwrap();

        let results_after = loaded.search(&query, 5).unwrap();

        // Results should be identical
        assert_eq!(results_before.len(), results_after.len());
        for (before, after) in results_before.iter().zip(results_after.iter()) {
            assert_eq!(before.0, after.0);
            assert!((before.1 - after.1).abs() < 1e-6);
        }
    }
}
