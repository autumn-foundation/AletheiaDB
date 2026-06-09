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
use crate::core::hasher::IdentityHasher;
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
use std::hash::BuildHasherDefault;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

/// Maximum number of results that can be requested in a search.
///
/// This prevents DoS attacks via excessive memory allocation.
const MAX_K: usize = 100_000;

/// Magic bytes for sparse index files: "ASPS" (AletheiaDB SParse Search).
const SPARSE_INDEX_MAGIC: [u8; 4] = [0x41, 0x53, 0x50, 0x53];

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
    inverted_index: DashMap<u32, Vec<Posting>>,
    /// Forward index: node_id -> stored vector (for removal and updates)
    vectors: DashMap<NodeId, StoredVector>,
    /// Number of vectors in the index
    count: AtomicUsize,
    /// Sum of all vector lengths (for BM25 avgdl)
    total_length: AtomicUsize,
    /// Document frequency: dimension -> count of documents containing it
    doc_freq: DashMap<u32, usize>,
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
            inverted_index: DashMap::with_capacity(capacity),
            vectors: DashMap::with_capacity(capacity),
            count: AtomicUsize::new(0),
            total_length: AtomicUsize::new(0),
            doc_freq: DashMap::with_capacity(capacity),
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
        let mut scores: HashMap<NodeId, f32, BuildHasherDefault<IdentityHasher>> =
            HashMap::default();
        // Magnitudes map is only used for cosine, but we always create it (cheap)
        let mut magnitudes: HashMap<NodeId, f32, BuildHasherDefault<IdentityHasher>> =
            HashMap::default();
        // Document lengths map is only used for BM25, but we always create it (cheap)
        let mut doc_lengths: HashMap<NodeId, f32, BuildHasherDefault<IdentityHasher>> =
            HashMap::default();
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
    /// - 4 bytes: Magic bytes "ASPS"
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
            inverted_index: DashMap::with_capacity(data.count as usize),
            vectors: DashMap::with_capacity(data.count as usize),
            count: AtomicUsize::new(data.count as usize),
            total_length: AtomicUsize::new(data.total_length as usize),
            doc_freq: DashMap::with_capacity(data.doc_freq.len()),
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
    let mut combined: HashMap<NodeId, f32, BuildHasherDefault<IdentityHasher>> = HashMap::default();

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

    let mut rrf_scores: HashMap<NodeId, f32, BuildHasherDefault<IdentityHasher>> =
        HashMap::default();

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
pub(crate) mod tests;
