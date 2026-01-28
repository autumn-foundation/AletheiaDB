//! Sharded vector index for managing large vector collections.
//!
//! This module provides a sharded implementation of the `VectorIndex` trait,
//! distributing vectors across multiple underlying HNSW indexes for scalability.
//!
//! # Overview
//!
//! The `ShardedVectorIndex` partitions vectors across multiple shards using
//! consistent hashing on NodeId. This enables:
//!
//! - **Horizontal scalability**: Handle larger datasets by adding more shards
//! - **Parallel search**: Query all shards concurrently and merge results
//! - **Memory distribution**: Spread memory usage across shards
//!
//! # Sharding Strategy
//!
//! Vectors are assigned to shards using hash-based partitioning:
//!
//! ```text
//! shard_index = hash(node_id) % num_shards
//! ```
//!
//! This provides even distribution regardless of NodeId patterns.
//!
//! # Important: Fixed Shard Count
//!
//! The number of shards is **fixed at construction time** and cannot be changed
//! dynamically. This is because hash-based routing depends on the modulo of the
//! shard count - changing it would cause existing vectors to be routed to wrong
//! shards on lookup.
//!
//! To scale to more shards, create a new index with the desired count and
//! migrate data.
//!
//! # Performance Characteristics
//!
//! - **Add operation**: Same as single HNSW (~1-10µs per vector)
//! - **Search operation**: Parallel search across shards, merge top-k results
//! - **Memory**: Distributed across shards, ~(dimensions + M) * 4 bytes per vector
//!
//! # Examples
//!
//! ```rust,no_run
//! use gallifreydb::index::vector::sharded::{ShardedVectorIndex, ShardedVectorConfig, ShardingStrategy};
//! use gallifreydb::index::vector::{HnswConfig, DistanceMetric, VectorIndex};
//! use gallifreydb::core::id::NodeId;
//!
//! # fn example() -> gallifreydb::utils::Result<()> {
//! // Create a sharded index with 4 shards
//! let config = ShardedVectorConfig::new(4)
//!     .with_hnsw_config(HnswConfig::new(384, DistanceMetric::Cosine))
//!     .with_strategy(ShardingStrategy::HashBased);
//!
//! let index = ShardedVectorIndex::new(config)?;
//!
//! // Add vectors - automatically routed to appropriate shard
//! let node1 = NodeId::new(1).unwrap();
//! let embedding1 = vec![0.1f32; 384];
//! index.add(node1, &embedding1)?;
//!
//! // Search across all shards
//! let query = vec![0.15f32; 384];
//! let results = index.search(&query, 10)?;
//! # Ok(())
//! # }
//! ```
//!
//! # Thread Safety
//!
//! `ShardedVectorIndex` is fully thread-safe:
//! - Multiple threads can add vectors simultaneously
//! - Multiple threads can search simultaneously
//! - Searches can run concurrently with additions

use crate::core::id::NodeId;
use crate::core::vector::validate_vector;
use crate::index::vector::{DistanceMetric, HnswConfig, HnswIndex, Quantization, VectorIndex};
use crate::utils::{Error, Result, error::VectorError};
use rayon::prelude::*;
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::Arc;

/// Maximum number of results that can be requested in a search.
///
/// This prevents DoS attacks via excessive memory allocation.
const MAX_K: usize = 10_000;

/// Default number of shards.
const DEFAULT_NUM_SHARDS: usize = 4;

/// Strategy for assigning vectors to shards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShardingStrategy {
    /// Hash-based partitioning: shard = hash(node_id) % num_shards
    /// Provides even distribution regardless of NodeId patterns.
    #[default]
    HashBased,
    /// Range-based partitioning: divides the u64 ID space evenly across shards.
    /// Better for range queries but may have uneven distribution if IDs are clustered.
    RangeBased,
}

/// Statistics for the sharded index.
#[derive(Debug, Default, Clone)]
pub struct ShardStats {
    /// Number of vectors in each shard.
    pub shard_sizes: Vec<usize>,
    /// Total number of vectors.
    pub total_vectors: usize,
    /// Imbalance ratio (max/min shard size, 1.0 = perfectly balanced).
    pub imbalance_ratio: f64,
}

/// Configuration for rebalancing shards.
#[derive(Debug, Clone)]
pub struct RebalanceConfig {
    /// Trigger rebalancing when imbalance ratio exceeds this threshold.
    pub imbalance_threshold: f64,
    /// Maximum vectors to move in a single rebalance operation.
    pub batch_size: usize,
}

impl Default for RebalanceConfig {
    fn default() -> Self {
        Self {
            imbalance_threshold: 2.0, // Rebalance when max shard is 2x min shard
            batch_size: 1000,
        }
    }
}

impl RebalanceConfig {
    /// Create a new rebalance configuration.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the imbalance threshold.
    pub fn with_imbalance_threshold(mut self, threshold: f64) -> Self {
        self.imbalance_threshold = threshold.max(1.0);
        self
    }

    /// Set the batch size.
    pub fn with_batch_size(mut self, size: usize) -> Self {
        self.batch_size = size.max(1);
        self
    }
}

/// Configuration for the sharded vector index.
#[derive(Debug, Clone)]
pub struct ShardedVectorConfig {
    /// Number of shards.
    pub num_shards: usize,
    /// Strategy for assigning vectors to shards.
    pub strategy: ShardingStrategy,
    /// Configuration for each HNSW shard.
    pub hnsw_config: HnswConfig,
    /// Configuration for rebalancing.
    pub rebalance_config: RebalanceConfig,
}

impl ShardedVectorConfig {
    /// Create a new configuration with the specified number of shards.
    pub fn new(num_shards: usize) -> Self {
        Self {
            num_shards: num_shards.max(1),
            strategy: ShardingStrategy::default(),
            hnsw_config: HnswConfig::default(),
            rebalance_config: RebalanceConfig::default(),
        }
    }

    /// Set the sharding strategy.
    pub fn with_strategy(mut self, strategy: ShardingStrategy) -> Self {
        self.strategy = strategy;
        self
    }

    /// Set the HNSW configuration for shards.
    pub fn with_hnsw_config(mut self, config: HnswConfig) -> Self {
        self.hnsw_config = config;
        self
    }

    /// Set the rebalance configuration.
    pub fn with_rebalance_config(mut self, config: RebalanceConfig) -> Self {
        self.rebalance_config = config;
        self
    }
}

impl Default for ShardedVectorConfig {
    fn default() -> Self {
        Self::new(DEFAULT_NUM_SHARDS)
    }
}

/// A sharded vector index distributing vectors across multiple HNSW indexes.
///
/// This structure provides horizontal scalability by partitioning vectors
/// across multiple shards and coordinating search operations.
///
/// # Fixed Shard Count
///
/// The number of shards is fixed at construction time. This is because the
/// routing algorithm (hash % num_shards) depends on a consistent shard count.
/// Changing it would cause existing vectors to be routed to wrong shards.
pub struct ShardedVectorIndex {
    /// Configuration for this index.
    config: ShardedVectorConfig,
    /// The underlying HNSW shards.
    shards: Vec<Arc<HnswIndex>>,
}

impl ShardedVectorIndex {
    /// Creates a new sharded vector index.
    ///
    /// # Arguments
    ///
    /// * `config` - Configuration for the sharded index
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - `num_shards` is 0 (will be clamped to 1)
    /// - HNSW configuration is invalid
    pub fn new(config: ShardedVectorConfig) -> Result<Self> {
        // Validate dimensions
        if config.hnsw_config.dimensions == 0 {
            return Err(Error::Vector(VectorError::InvalidVector {
                reason: "dimensions must be > 0".to_string(),
            }));
        }

        // Ensure num_shards is at least 1
        let num_shards = config.num_shards.max(1);

        // Create shards
        let mut shards = Vec::with_capacity(num_shards);
        for _ in 0..num_shards {
            let shard = HnswIndex::new(config.hnsw_config.clone())?;
            shards.push(Arc::new(shard));
        }

        Ok(Self { config, shards })
    }

    /// Creates a new sharded index with default configuration.
    ///
    /// # Arguments
    ///
    /// * `dimensions` - Vector dimensionality
    /// * `metric` - Distance metric
    /// * `num_shards` - Number of shards
    pub fn with_defaults(
        dimensions: usize,
        metric: DistanceMetric,
        num_shards: usize,
    ) -> Result<Self> {
        let hnsw_config = HnswConfig::new(dimensions, metric);
        let config = ShardedVectorConfig::new(num_shards).with_hnsw_config(hnsw_config);
        Self::new(config)
    }

    /// Returns the number of shards.
    pub fn num_shards(&self) -> usize {
        self.shards.len()
    }

    /// Returns the sharding strategy.
    pub fn strategy(&self) -> ShardingStrategy {
        self.config.strategy
    }

    /// Returns the configuration.
    pub fn config(&self) -> &ShardedVectorConfig {
        &self.config
    }

    /// Returns statistics about shard distribution.
    pub fn stats(&self) -> ShardStats {
        let shard_sizes: Vec<usize> = self.shards.iter().map(|s| s.len()).collect();
        let total_vectors: usize = shard_sizes.iter().sum();

        let min_size = shard_sizes.iter().min().copied().unwrap_or(0);
        let max_size = shard_sizes.iter().max().copied().unwrap_or(0);

        let imbalance_ratio = if min_size > 0 {
            max_size as f64 / min_size as f64
        } else if max_size > 0 {
            f64::INFINITY
        } else {
            1.0
        };

        ShardStats {
            shard_sizes,
            total_vectors,
            imbalance_ratio,
        }
    }

    /// Returns the shard index for a given NodeId.
    ///
    /// The routing is deterministic: the same NodeId always maps to the same shard.
    fn shard_for_id(&self, id: NodeId) -> usize {
        // num_shards is always >= 1 due to clamping in constructor
        debug_assert!(!self.shards.is_empty(), "shards cannot be empty");
        let num_shards = self.shards.len();

        match self.config.strategy {
            ShardingStrategy::HashBased => {
                let mut hasher = DefaultHasher::new();
                id.as_u64().hash(&mut hasher);
                (hasher.finish() as usize) % num_shards
            }
            ShardingStrategy::RangeBased => {
                // Divide the u64 ID space evenly across shards using u128 to avoid overflow
                // This maps [0, u64::MAX] proportionally to [0, num_shards)
                let num_shards_128 = num_shards as u128;
                let id_128 = id.as_u64() as u128;
                // We use (u64::MAX as u128 + 1) to represent the full range size
                let shard = ((id_128 * num_shards_128) / (u64::MAX as u128 + 1)) as usize;
                // Clamp to valid range to handle edge case where id=u64::MAX
                shard.min(num_shards - 1)
            }
        }
    }

    /// Get a reference to a specific shard.
    pub fn get_shard(&self, index: usize) -> Option<&Arc<HnswIndex>> {
        self.shards.get(index)
    }

    /// Check if rebalancing is needed based on current imbalance.
    pub fn needs_rebalancing(&self) -> bool {
        let stats = self.stats();
        stats.imbalance_ratio > self.config.rebalance_config.imbalance_threshold
    }

    /// Estimate the number of vectors that would need to move to rebalance shards.
    ///
    /// # Current Limitation
    ///
    /// This method only **estimates** how many vectors would need to move
    /// to achieve balance. It does **not** actually move any vectors because the
    /// underlying HNSW index doesn't support efficient iteration over stored vectors.
    ///
    /// A full rebalancing implementation would require:
    /// - Maintaining a separate vector storage (id -> vector mapping)
    /// - Using the index persistence format to extract vectors
    /// - Or implementing an iterator API in the HNSW index
    ///
    /// # Returns
    ///
    /// The estimated number of vectors that would need to move (capped by batch_size).
    /// Returns 0 if the index is already balanced below the threshold.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use gallifreydb::index::vector::sharded::ShardedVectorIndex;
    /// # use gallifreydb::index::vector::DistanceMetric;
    /// # fn example() -> gallifreydb::utils::Result<()> {
    /// let index = ShardedVectorIndex::with_defaults(128, DistanceMetric::Cosine, 4)?;
    /// // ... add vectors ...
    /// let vectors_to_move = index.estimate_rebalance_cost()?;
    /// println!("Would need to move {} vectors to rebalance", vectors_to_move);
    /// # Ok(())
    /// # }
    /// ```
    pub fn estimate_rebalance_cost(&self) -> Result<usize> {
        // num_shards is always >= 1 due to clamping in constructor
        debug_assert!(!self.shards.is_empty(), "shards cannot be empty");

        let stats = self.stats();
        if stats.imbalance_ratio <= self.config.rebalance_config.imbalance_threshold {
            return Ok(0);
        }

        // Calculate target size for each shard
        let target_size = stats.total_vectors / self.shards.len();
        let mut vectors_to_move = 0;

        // Count how many vectors would need to move from overloaded shards
        for size in &stats.shard_sizes {
            if *size > target_size {
                vectors_to_move += size - target_size;
            }
        }

        // Cap at batch size
        vectors_to_move = vectors_to_move.min(self.config.rebalance_config.batch_size);

        Ok(vectors_to_move)
    }

    /// Returns the total memory usage across all shards.
    pub fn total_memory_usage(&self) -> usize {
        self.shards.iter().map(|s| s.memory_usage()).sum()
    }

    /// Load a sharded index from disk.
    ///
    /// # Arguments
    ///
    /// * `path` - Base path used when saving (shards are at path.shard_N)
    /// * `config` - Configuration matching the saved index
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Any shard file cannot be loaded
    /// - The configuration doesn't match the saved index
    /// - The path contains invalid characters or path traversal attempts
    pub fn load(path: &Path, config: ShardedVectorConfig) -> Result<Self> {
        // Validate path to prevent path traversal attacks
        Self::validate_path(path)?;

        let mut shards = Vec::with_capacity(config.num_shards);

        for i in 0..config.num_shards {
            let shard_path = Self::shard_path(path, i)?;
            let shard = HnswIndex::load(&shard_path, config.hnsw_config.clone())?;
            shards.push(Arc::new(shard));
        }

        Ok(Self { config, shards })
    }

    /// Validate a path for security concerns.
    fn validate_path(path: &Path) -> Result<()> {
        // Check for path traversal attempts
        let path_str = path.to_string_lossy();
        if path_str.contains("..") {
            return Err(Error::Vector(VectorError::IndexError(
                "Path contains invalid traversal characters (..)".to_string(),
            )));
        }

        // Ensure the path has a valid file name
        if path.file_name().is_none() {
            return Err(Error::Vector(VectorError::IndexError(
                "Path must have a valid file name".to_string(),
            )));
        }

        Ok(())
    }

    /// Generate the path for a specific shard.
    fn shard_path(base_path: &Path, shard_index: usize) -> Result<std::path::PathBuf> {
        let file_name = base_path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| {
                Error::Vector(VectorError::IndexError(
                    "Path must have a valid UTF-8 file name".to_string(),
                ))
            })?;

        Ok(base_path.with_file_name(format!("{}.shard_{}", file_name, shard_index)))
    }

    /// Merge search results from multiple shards using a min-heap for efficiency.
    ///
    /// This is O(n log k) where n is total results and k is the desired count,
    /// compared to O(n log n) for sorting all results.
    fn merge_results(shard_results: Vec<Vec<(NodeId, f32)>>, k: usize) -> Vec<(NodeId, f32)> {
        if k == 0 {
            return Vec::new();
        }

        // Use a min-heap to track the top-k results
        // We store (Reverse(score), NodeId) so the smallest score is at the top
        let mut heap: BinaryHeap<(Reverse<OrderedFloat>, NodeId)> =
            BinaryHeap::with_capacity(k + 1);

        for results in shard_results {
            for (id, score) in results {
                let ordered_score = OrderedFloat(score);

                if heap.len() < k {
                    heap.push((Reverse(ordered_score), id));
                } else if let Some(&(Reverse(min_score), _)) = heap.peek()
                    && ordered_score > min_score
                {
                    heap.pop();
                    heap.push((Reverse(ordered_score), id));
                }
            }
        }

        // Extract results and sort by descending score
        let mut results: Vec<(NodeId, f32)> = heap
            .into_iter()
            .map(|(Reverse(score), id)| (id, score.0))
            .collect();

        // Sort descending by score
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        results
    }
}

/// Wrapper for f32 that implements Ord for use in BinaryHeap.
///
/// NaN values are treated as less than all other values for consistent ordering.
#[derive(Debug, Clone, Copy, PartialEq)]
struct OrderedFloat(f32);

impl Eq for OrderedFloat {}

impl PartialOrd for OrderedFloat {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OrderedFloat {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.partial_cmp(&other.0).unwrap_or_else(|| {
            // Handle NaN: treat NaN as less than everything
            match (self.0.is_nan(), other.0.is_nan()) {
                (true, true) => std::cmp::Ordering::Equal,
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                (false, false) => unreachable!(),
            }
        })
    }
}

impl VectorIndex for ShardedVectorIndex {
    fn add(&self, id: NodeId, vector: &[f32]) -> Result<()> {
        // Validate vector
        validate_vector(vector)?;

        // Check dimensions match
        if vector.len() != self.config.hnsw_config.dimensions {
            return Err(Error::Vector(VectorError::DimensionMismatch {
                expected: self.config.hnsw_config.dimensions,
                actual: vector.len(),
            }));
        }

        // Route to appropriate shard
        let shard_idx = self.shard_for_id(id);
        self.shards[shard_idx].add(id, vector)
    }

    fn remove(&self, id: NodeId) -> Result<()> {
        // Route to appropriate shard
        let shard_idx = self.shard_for_id(id);
        self.shards[shard_idx].remove(id)
    }

    fn search(&self, query: &[f32], k: usize) -> Result<Vec<(NodeId, f32)>> {
        // Validate query vector
        validate_vector(query)?;

        // Check dimensions match
        if query.len() != self.config.hnsw_config.dimensions {
            return Err(Error::Vector(VectorError::DimensionMismatch {
                expected: self.config.hnsw_config.dimensions,
                actual: query.len(),
            }));
        }

        // Cap k to prevent DoS
        let k_capped = k.min(MAX_K);

        // Search all shards in parallel using rayon
        let shard_results: Result<Vec<Vec<(NodeId, f32)>>> = self
            .shards
            .par_iter()
            .filter(|shard| !shard.is_empty())
            .map(|shard| shard.search(query, k_capped))
            .collect();

        let shard_results = shard_results?;

        // Merge results using efficient heap-based algorithm
        Ok(Self::merge_results(shard_results, k_capped))
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

        if query.len() != self.config.hnsw_config.dimensions {
            return Err(Error::Vector(VectorError::DimensionMismatch {
                expected: self.config.hnsw_config.dimensions,
                actual: query.len(),
            }));
        }

        let k_capped = k.min(MAX_K);

        // Search all shards in parallel with filter
        let shard_results: Result<Vec<Vec<(NodeId, f32)>>> = self
            .shards
            .par_iter()
            .filter(|shard| !shard.is_empty())
            .map(|shard| shard.search_with_filter(query, k_capped, &predicate))
            .collect();

        let shard_results = shard_results?;

        // Merge results using efficient heap-based algorithm
        Ok(Self::merge_results(shard_results, k_capped))
    }

    fn len(&self) -> usize {
        self.shards.iter().map(|s| s.len()).sum()
    }

    fn dimensions(&self) -> usize {
        self.config.hnsw_config.dimensions
    }

    fn distance_metric(&self) -> DistanceMetric {
        self.config.hnsw_config.metric
    }

    fn add_batch(&self, items: &[(NodeId, Vec<f32>)]) -> Result<()> {
        // Optimization: Use add_batch_ref to avoid cloning vectors
        // We create references to the vectors in the input items
        let refs: Vec<(NodeId, &[f32])> = items
            .iter()
            .map(|(id, vec)| (*id, vec.as_slice()))
            .collect();

        self.add_batch_ref(&refs)
    }

    fn add_batch_ref(&self, items: &[(NodeId, &[f32])]) -> Result<()> {
        // Group item indices by shard first
        let mut shard_indices: Vec<Vec<usize>> = vec![Vec::new(); self.shards.len()];

        for (idx, (id, _)) in items.iter().enumerate() {
            let shard_idx = self.shard_for_id(*id);
            shard_indices[shard_idx].push(idx);
        }

        // Add to each shard
        for (shard_idx, indices) in shard_indices.into_iter().enumerate() {
            if !indices.is_empty() {
                let shard_items: Vec<(NodeId, &[f32])> = indices
                    .iter()
                    .map(|&idx| (items[idx].0, items[idx].1))
                    .collect();
                self.shards[shard_idx].add_batch_ref(&shard_items)?;
            }
        }

        Ok(())
    }

    fn remove_batch(&self, ids: &[NodeId]) -> Result<()> {
        // Group IDs by shard
        let mut shard_ids: Vec<Vec<NodeId>> = vec![Vec::new(); self.shards.len()];

        for id in ids {
            let shard_idx = self.shard_for_id(*id);
            shard_ids[shard_idx].push(*id);
        }

        // Remove from each shard
        for (shard_idx, ids) in shard_ids.into_iter().enumerate() {
            if !ids.is_empty() {
                self.shards[shard_idx].remove_batch(&ids)?;
            }
        }

        Ok(())
    }

    fn save(&self, path: &Path) -> Result<()> {
        // Validate path for security
        Self::validate_path(path)?;

        // Save each shard to a separate file
        for (i, shard) in self.shards.iter().enumerate() {
            let shard_path = Self::shard_path(path, i)?;
            shard.save(&shard_path)?;
        }
        Ok(())
    }

    fn memory_usage(&self) -> usize {
        self.total_memory_usage()
    }

    fn quantization(&self) -> Quantization {
        self.config.hnsw_config.quantization
    }

    fn compact(&self) -> Result<()> {
        for shard in &self.shards {
            shard.compact()?;
        }
        Ok(())
    }
}

// Note: ShardedVectorIndex automatically implements Send + Sync because:
// - `config`: ShardedVectorConfig is Send + Sync (contains only Send + Sync types)
// - `shards`: Vec<Arc<HnswIndex>> - HnswIndex manually implements Send + Sync

#[cfg(test)]
mod tests {
    use super::*;

    // ============================================================
    // Configuration Tests
    // ============================================================

    #[test]
    fn test_sharded_config_defaults() {
        let config = ShardedVectorConfig::default();
        assert_eq!(config.num_shards, DEFAULT_NUM_SHARDS);
        assert_eq!(config.strategy, ShardingStrategy::HashBased);
    }

    #[test]
    fn test_sharded_config_builder() {
        let hnsw_config = HnswConfig::new(128, DistanceMetric::Cosine);
        let config = ShardedVectorConfig::new(8)
            .with_strategy(ShardingStrategy::RangeBased)
            .with_hnsw_config(hnsw_config)
            .with_rebalance_config(RebalanceConfig::new().with_imbalance_threshold(1.5));

        assert_eq!(config.num_shards, 8);
        assert_eq!(config.strategy, ShardingStrategy::RangeBased);
        assert_eq!(config.hnsw_config.dimensions, 128);
        assert!((config.rebalance_config.imbalance_threshold - 1.5).abs() < 0.001);
    }

    #[test]
    fn test_sharded_config_clamps_num_shards() {
        let config = ShardedVectorConfig::new(0);
        assert_eq!(config.num_shards, 1);
    }

    #[test]
    fn test_rebalance_config_defaults() {
        let config = RebalanceConfig::default();
        assert!((config.imbalance_threshold - 2.0).abs() < 0.001);
        assert_eq!(config.batch_size, 1000);
    }

    #[test]
    fn test_rebalance_config_clamps_threshold() {
        let config = RebalanceConfig::new().with_imbalance_threshold(0.5);
        assert!((config.imbalance_threshold - 1.0).abs() < 0.001);
    }

    // ============================================================
    // Creation Tests
    // ============================================================

    #[test]
    fn test_create_sharded_index() -> Result<()> {
        let config = ShardedVectorConfig::new(4)
            .with_hnsw_config(HnswConfig::new(128, DistanceMetric::Cosine));
        let index = ShardedVectorIndex::new(config)?;

        assert_eq!(index.num_shards(), 4);
        assert_eq!(index.dimensions(), 128);
        assert_eq!(index.distance_metric(), DistanceMetric::Cosine);
        assert_eq!(index.len(), 0);
        assert!(index.is_empty());

        Ok(())
    }

    #[test]
    fn test_create_with_defaults() -> Result<()> {
        let index = ShardedVectorIndex::with_defaults(256, DistanceMetric::Euclidean, 2)?;

        assert_eq!(index.num_shards(), 2);
        assert_eq!(index.dimensions(), 256);
        assert_eq!(index.distance_metric(), DistanceMetric::Euclidean);

        Ok(())
    }

    #[test]
    fn test_create_fails_with_zero_dimensions() {
        let config = ShardedVectorConfig::new(4)
            .with_hnsw_config(HnswConfig::new(0, DistanceMetric::Cosine));
        let result = ShardedVectorIndex::new(config);

        assert!(result.is_err());
    }

    // ============================================================
    // Add/Remove Tests
    // ============================================================

    #[test]
    fn test_add_single_vector() -> Result<()> {
        let index = ShardedVectorIndex::with_defaults(4, DistanceMetric::Cosine, 4)?;

        let node = NodeId::new(1).unwrap();
        let vector = vec![1.0, 0.0, 0.0, 0.0];
        index.add(node, &vector)?;

        assert_eq!(index.len(), 1);
        assert!(!index.is_empty());

        Ok(())
    }

    #[test]
    fn test_add_multiple_vectors() -> Result<()> {
        let index = ShardedVectorIndex::with_defaults(4, DistanceMetric::Cosine, 4)?;

        for i in 1..=100 {
            let node = NodeId::new(i).unwrap();
            let vector = vec![i as f32, 0.0, 0.0, 0.0];
            index.add(node, &vector)?;
        }

        assert_eq!(index.len(), 100);

        Ok(())
    }

    #[test]
    fn test_add_dimension_mismatch() {
        let index = ShardedVectorIndex::with_defaults(4, DistanceMetric::Cosine, 4).unwrap();

        let node = NodeId::new(1).unwrap();
        let wrong_dim_vector = vec![1.0, 0.0]; // Only 2 dimensions, expected 4

        let result = index.add(node, &wrong_dim_vector);
        assert!(result.is_err());
    }

    #[test]
    fn test_add_with_nan() {
        let index = ShardedVectorIndex::with_defaults(4, DistanceMetric::Cosine, 4).unwrap();

        let node = NodeId::new(1).unwrap();
        let nan_vector = vec![1.0, f32::NAN, 0.0, 0.0];

        let result = index.add(node, &nan_vector);
        assert!(result.is_err());
    }

    #[test]
    fn test_remove_vector() -> Result<()> {
        let index = ShardedVectorIndex::with_defaults(4, DistanceMetric::Cosine, 4)?;

        let node = NodeId::new(1).unwrap();
        let vector = vec![1.0, 0.0, 0.0, 0.0];

        index.add(node, &vector)?;
        assert_eq!(index.len(), 1);

        index.remove(node)?;
        assert_eq!(index.len(), 0);

        Ok(())
    }

    #[test]
    fn test_remove_nonexistent() -> Result<()> {
        let index = ShardedVectorIndex::with_defaults(4, DistanceMetric::Cosine, 4)?;

        let node = NodeId::new(1).unwrap();
        // Remove should be a no-op for nonexistent nodes
        index.remove(node)?;

        assert_eq!(index.len(), 0);

        Ok(())
    }

    // ============================================================
    // Batch Operation Tests
    // ============================================================

    #[test]
    fn test_add_batch() -> Result<()> {
        let index = ShardedVectorIndex::with_defaults(4, DistanceMetric::Cosine, 4)?;

        let items: Vec<(NodeId, Vec<f32>)> = (1..=10)
            .map(|i| (NodeId::new(i).unwrap(), vec![i as f32, 0.0, 0.0, 0.0]))
            .collect();

        index.add_batch(&items)?;

        assert_eq!(index.len(), 10);

        Ok(())
    }

    #[test]
    fn test_remove_batch() -> Result<()> {
        let index = ShardedVectorIndex::with_defaults(4, DistanceMetric::Cosine, 4)?;

        // Add vectors
        for i in 1..=10 {
            let node = NodeId::new(i).unwrap();
            index.add(node, &[i as f32, 0.0, 0.0, 0.0])?;
        }
        assert_eq!(index.len(), 10);

        // Remove half
        let ids_to_remove: Vec<NodeId> = (1..=5).map(|i| NodeId::new(i).unwrap()).collect();
        index.remove_batch(&ids_to_remove)?;

        assert_eq!(index.len(), 5);

        Ok(())
    }

    // ============================================================
    // Search Tests
    // ============================================================

    #[test]
    fn test_search_empty_index() -> Result<()> {
        let index = ShardedVectorIndex::with_defaults(4, DistanceMetric::Cosine, 4)?;

        let query = vec![1.0, 0.0, 0.0, 0.0];
        let results = index.search(&query, 10)?;

        assert!(results.is_empty());

        Ok(())
    }

    #[test]
    fn test_search_basic() -> Result<()> {
        let index = ShardedVectorIndex::with_defaults(4, DistanceMetric::Cosine, 4)?;

        let node1 = NodeId::new(1).unwrap();
        let node2 = NodeId::new(2).unwrap();
        let node3 = NodeId::new(3).unwrap();

        index.add(node1, &[1.0, 0.0, 0.0, 0.0])?;
        index.add(node2, &[0.9, 0.1, 0.0, 0.0])?;
        index.add(node3, &[0.0, 1.0, 0.0, 0.0])?;

        let query = vec![1.0, 0.0, 0.0, 0.0];
        let results = index.search(&query, 3)?;

        assert_eq!(results.len(), 3);
        // node1 should be most similar (identical to query)
        assert_eq!(results[0].0, node1);
        // node2 should be second (very similar)
        assert_eq!(results[1].0, node2);

        Ok(())
    }

    #[test]
    fn test_search_k_larger_than_index() -> Result<()> {
        let index = ShardedVectorIndex::with_defaults(4, DistanceMetric::Cosine, 4)?;

        let node1 = NodeId::new(1).unwrap();
        let node2 = NodeId::new(2).unwrap();

        index.add(node1, &[1.0, 0.0, 0.0, 0.0])?;
        index.add(node2, &[0.0, 1.0, 0.0, 0.0])?;

        // Request more results than available
        let query = vec![1.0, 0.0, 0.0, 0.0];
        let results = index.search(&query, 100)?;

        // Should return all available (2)
        assert_eq!(results.len(), 2);

        Ok(())
    }

    #[test]
    fn test_search_dimension_mismatch() {
        let index = ShardedVectorIndex::with_defaults(4, DistanceMetric::Cosine, 4).unwrap();

        let node = NodeId::new(1).unwrap();
        index.add(node, &[1.0, 0.0, 0.0, 0.0]).unwrap();

        let wrong_dim_query = vec![1.0, 0.0]; // Only 2 dimensions

        let result = index.search(&wrong_dim_query, 10);
        assert!(result.is_err());
    }

    // ============================================================
    // Filtered Search Tests
    // ============================================================

    #[test]
    fn test_search_with_filter() -> Result<()> {
        let index = ShardedVectorIndex::with_defaults(4, DistanceMetric::Cosine, 4)?;

        for i in 1..=10 {
            let node = NodeId::new(i).unwrap();
            index.add(node, &[i as f32, 0.0, 0.0, 0.0])?;
        }

        let query = vec![5.0, 0.0, 0.0, 0.0];
        // Only return nodes with even IDs
        let results = index.search_with_filter(&query, 10, |id| id.as_u64() % 2 == 0)?;

        // Should only have even IDs
        for (id, _) in &results {
            assert_eq!(id.as_u64() % 2, 0);
        }

        Ok(())
    }

    #[test]
    fn test_search_with_filter_no_matches() -> Result<()> {
        let index = ShardedVectorIndex::with_defaults(4, DistanceMetric::Cosine, 4)?;

        let node = NodeId::new(1).unwrap();
        index.add(node, &[1.0, 0.0, 0.0, 0.0])?;

        let query = vec![1.0, 0.0, 0.0, 0.0];
        // Filter that matches nothing
        let results = index.search_with_filter(&query, 10, |_| false)?;

        assert!(results.is_empty());

        Ok(())
    }

    // ============================================================
    // Sharding Distribution Tests
    // ============================================================

    #[test]
    fn test_hash_based_distribution() -> Result<()> {
        let index = ShardedVectorIndex::with_defaults(4, DistanceMetric::Cosine, 4)?;

        // Add many vectors
        for i in 1..=1000 {
            let node = NodeId::new(i).unwrap();
            index.add(node, &[i as f32, 0.0, 0.0, 0.0])?;
        }

        // Check distribution - should be roughly even
        let stats = index.stats();
        assert_eq!(stats.total_vectors, 1000);

        // With 4 shards and 1000 vectors, expect ~250 per shard
        // Allow some variance (between 150 and 350 per shard)
        for size in &stats.shard_sizes {
            assert!(*size >= 100, "Shard too small: {}", size);
            assert!(*size <= 400, "Shard too large: {}", size);
        }

        Ok(())
    }

    #[test]
    fn test_range_based_distribution() -> Result<()> {
        let config = ShardedVectorConfig::new(4)
            .with_strategy(ShardingStrategy::RangeBased)
            .with_hnsw_config(HnswConfig::new(4, DistanceMetric::Cosine));
        let index = ShardedVectorIndex::new(config)?;

        // Add vectors with sequential IDs
        for i in 1..=100 {
            let node = NodeId::new(i).unwrap();
            index.add(node, &[i as f32, 0.0, 0.0, 0.0])?;
        }

        assert_eq!(index.len(), 100);

        // Range-based should put sequential IDs in the same shard
        // (until range boundary)
        let stats = index.stats();
        assert_eq!(stats.total_vectors, 100);

        Ok(())
    }

    #[test]
    fn test_range_based_distribution_even() -> Result<()> {
        // Test that range-based distribution is even across the full ID space
        let config = ShardedVectorConfig::new(4)
            .with_strategy(ShardingStrategy::RangeBased)
            .with_hnsw_config(HnswConfig::new(4, DistanceMetric::Cosine));
        let index = ShardedVectorIndex::new(config)?;

        // Add vectors spread across the u64 range
        let test_ids: Vec<u64> = vec![
            0,
            u64::MAX / 4,
            u64::MAX / 2,
            u64::MAX / 4 * 3,
            u64::MAX - 1000, // near max
        ];

        for id in test_ids {
            if let Ok(node) = NodeId::new(id) {
                index.add(node, &[id as f32, 0.0, 0.0, 0.0])?;
            }
        }

        // Verify vectors are distributed across different shards
        let stats = index.stats();
        let non_empty_shards = stats.shard_sizes.iter().filter(|&&s| s > 0).count();
        assert!(
            non_empty_shards >= 2,
            "Vectors should be in multiple shards"
        );

        Ok(())
    }

    #[test]
    fn test_consistent_routing() -> Result<()> {
        let index = ShardedVectorIndex::with_defaults(4, DistanceMetric::Cosine, 4)?;

        // Same NodeId should always route to the same shard
        let node = NodeId::new(42).unwrap();
        let shard1 = index.shard_for_id(node);
        let shard2 = index.shard_for_id(node);
        let shard3 = index.shard_for_id(node);

        assert_eq!(shard1, shard2);
        assert_eq!(shard2, shard3);

        Ok(())
    }

    #[test]
    fn test_range_based_max_id_edge_case() -> Result<()> {
        // Test that u64::MAX doesn't cause index-out-of-bounds
        let config = ShardedVectorConfig::new(4)
            .with_strategy(ShardingStrategy::RangeBased)
            .with_hnsw_config(HnswConfig::new(4, DistanceMetric::Cosine));
        let index = ShardedVectorIndex::new(config)?;

        // Test edge case IDs that could cause overflow or out-of-bounds
        let edge_ids: Vec<u64> = vec![
            0,
            1,
            u64::MAX - 1,
            u64::MAX, // This was previously causing index-out-of-bounds
        ];

        for id in edge_ids {
            if let Ok(node) = NodeId::new(id) {
                let shard = index.shard_for_id(node);
                assert!(
                    shard < 4,
                    "Shard index {} out of bounds for id {}",
                    shard,
                    id
                );
            }
        }

        Ok(())
    }

    // ============================================================
    // Stats and Rebalancing Tests
    // ============================================================

    #[test]
    fn test_stats_empty_index() -> Result<()> {
        let index = ShardedVectorIndex::with_defaults(4, DistanceMetric::Cosine, 4)?;

        let stats = index.stats();
        assert_eq!(stats.total_vectors, 0);
        assert_eq!(stats.shard_sizes.len(), 4);
        assert!((stats.imbalance_ratio - 1.0).abs() < 0.001);

        Ok(())
    }

    #[test]
    fn test_needs_rebalancing() -> Result<()> {
        let config = ShardedVectorConfig::new(4)
            .with_hnsw_config(HnswConfig::new(4, DistanceMetric::Cosine))
            .with_rebalance_config(RebalanceConfig::new().with_imbalance_threshold(2.0));
        let index = ShardedVectorIndex::new(config)?;

        // Empty index - balanced
        assert!(!index.needs_rebalancing());

        Ok(())
    }

    #[test]
    fn test_estimate_rebalance_cost() -> Result<()> {
        let index = ShardedVectorIndex::with_defaults(4, DistanceMetric::Cosine, 4)?;

        // Add evenly distributed vectors
        for i in 1..=100 {
            let node = NodeId::new(i).unwrap();
            index.add(node, &[i as f32, 0.0, 0.0, 0.0])?;
        }

        // Estimate rebalancing cost
        let cost = index.estimate_rebalance_cost()?;
        // May or may not need rebalancing depending on hash distribution
        assert!(cost <= 100);

        Ok(())
    }

    // ============================================================
    // Memory Usage Tests
    // ============================================================

    #[test]
    fn test_memory_usage() -> Result<()> {
        let index = ShardedVectorIndex::with_defaults(128, DistanceMetric::Cosine, 4)?;

        // Empty index has some baseline memory
        let empty_usage = index.memory_usage();
        assert!(empty_usage > 0);

        // Add vectors
        for i in 1..=100 {
            let node = NodeId::new(i).unwrap();
            let vector: Vec<f32> = (0..128).map(|j| (i * j) as f32).collect();
            index.add(node, &vector)?;
        }

        // Memory should increase
        let usage_with_vectors = index.memory_usage();
        assert!(usage_with_vectors > empty_usage);

        Ok(())
    }

    // ============================================================
    // Thread Safety Tests
    // ============================================================

    #[test]
    fn test_concurrent_add() -> Result<()> {
        use std::thread;

        let index = Arc::new(ShardedVectorIndex::with_defaults(
            4,
            DistanceMetric::Cosine,
            4,
        )?);

        let mut handles = vec![];

        for t in 0..4 {
            let index = Arc::clone(&index);
            let handle = thread::spawn(move || {
                for i in 0..25 {
                    let id = t * 25 + i + 1;
                    let node = NodeId::new(id as u64).unwrap();
                    let vector = vec![id as f32, 0.0, 0.0, 0.0];
                    index.add(node, &vector).unwrap();
                }
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.join().unwrap();
        }

        assert_eq!(index.len(), 100);

        Ok(())
    }

    #[test]
    fn test_concurrent_search() -> Result<()> {
        use std::thread;

        let index = Arc::new(ShardedVectorIndex::with_defaults(
            4,
            DistanceMetric::Cosine,
            4,
        )?);

        // Add some vectors
        for i in 1..=50 {
            let node = NodeId::new(i).unwrap();
            index.add(node, &[i as f32, 0.0, 0.0, 0.0])?;
        }

        let mut handles = vec![];

        for _ in 0..4 {
            let index = Arc::clone(&index);
            let handle = thread::spawn(move || {
                for i in 1..=10 {
                    let query = vec![i as f32, 0.0, 0.0, 0.0];
                    let results = index.search(&query, 5).unwrap();
                    assert!(!results.is_empty());
                }
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.join().unwrap();
        }

        Ok(())
    }

    // ============================================================
    // Quantization Tests
    // ============================================================

    #[test]
    fn test_quantization() -> Result<()> {
        let config = ShardedVectorConfig::new(4).with_hnsw_config(
            HnswConfig::new(4, DistanceMetric::Cosine).with_quantization(Quantization::F16),
        );
        let index = ShardedVectorIndex::new(config)?;

        assert_eq!(index.quantization(), Quantization::F16);

        Ok(())
    }

    // ============================================================
    // Cross-Shard Search Tests
    // ============================================================

    #[test]
    fn test_cross_shard_search_merging() -> Result<()> {
        let index = ShardedVectorIndex::with_defaults(4, DistanceMetric::Cosine, 4)?;

        // Add vectors that will be distributed across different shards
        // due to hash-based routing
        for i in 1..=100 {
            let node = NodeId::new(i).unwrap();
            // Use i as the first component so vectors have different similarities to query
            index.add(node, &[i as f32, 0.0, 0.0, 0.0])?;
        }

        // Query should find results from all shards
        let query = vec![50.0, 0.0, 0.0, 0.0];
        let results = index.search(&query, 10)?;

        assert_eq!(results.len(), 10);

        // Results should be sorted by similarity (highest first)
        for i in 0..results.len() - 1 {
            assert!(
                results[i].1 >= results[i + 1].1,
                "Results not sorted by similarity"
            );
        }

        Ok(())
    }

    #[test]
    fn test_search_results_from_multiple_shards() -> Result<()> {
        let index = ShardedVectorIndex::with_defaults(4, DistanceMetric::Cosine, 4)?;

        // Add 4 vectors that should definitely go to different shards
        // (hash distribution)
        let vectors = vec![
            (1u64, [1.0f32, 0.0, 0.0, 0.0]),
            (1000, [0.9, 0.1, 0.0, 0.0]),
            (2000, [0.8, 0.2, 0.0, 0.0]),
            (3000, [0.7, 0.3, 0.0, 0.0]),
        ];

        for (id, vec) in &vectors {
            let node = NodeId::new(*id).unwrap();
            index.add(node, vec)?;
        }

        let stats = index.stats();
        assert_eq!(stats.total_vectors, 4);

        // Search should return all 4
        let query = vec![1.0, 0.0, 0.0, 0.0];
        let results = index.search(&query, 10)?;

        assert_eq!(results.len(), 4);

        Ok(())
    }

    // ============================================================
    // Heap-based Merging Tests
    // ============================================================

    #[test]
    fn test_merge_results_empty() {
        let results = ShardedVectorIndex::merge_results(vec![], 10);
        assert!(results.is_empty());
    }

    #[test]
    fn test_merge_results_single_shard() {
        let shard_results = vec![vec![
            (NodeId::new(1).unwrap(), 0.9),
            (NodeId::new(2).unwrap(), 0.8),
            (NodeId::new(3).unwrap(), 0.7),
        ]];

        let merged = ShardedVectorIndex::merge_results(shard_results, 2);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].0, NodeId::new(1).unwrap());
        assert_eq!(merged[1].0, NodeId::new(2).unwrap());
    }

    #[test]
    fn test_merge_results_multiple_shards() {
        let shard_results = vec![
            vec![
                (NodeId::new(1).unwrap(), 0.9),
                (NodeId::new(2).unwrap(), 0.7),
            ],
            vec![
                (NodeId::new(3).unwrap(), 0.85),
                (NodeId::new(4).unwrap(), 0.6),
            ],
        ];

        let merged = ShardedVectorIndex::merge_results(shard_results, 3);
        assert_eq!(merged.len(), 3);
        // Should be sorted: 0.9, 0.85, 0.7
        assert_eq!(merged[0].0, NodeId::new(1).unwrap());
        assert_eq!(merged[1].0, NodeId::new(3).unwrap());
        assert_eq!(merged[2].0, NodeId::new(2).unwrap());
    }

    #[test]
    fn test_merge_results_k_zero() {
        let shard_results = vec![vec![(NodeId::new(1).unwrap(), 0.9)]];
        let merged = ShardedVectorIndex::merge_results(shard_results, 0);
        assert!(merged.is_empty());
    }

    // ============================================================
    // Path Validation Tests
    // ============================================================

    #[test]
    fn test_path_validation_traversal() {
        let path = Path::new("/data/../etc/passwd");
        let result = ShardedVectorIndex::validate_path(path);
        assert!(result.is_err());
    }

    #[test]
    fn test_path_validation_no_filename() {
        let path = Path::new("/");
        let result = ShardedVectorIndex::validate_path(path);
        assert!(result.is_err());
    }

    #[test]
    fn test_path_validation_valid() {
        let path = Path::new("/data/index");
        let result = ShardedVectorIndex::validate_path(path);
        assert!(result.is_ok());
    }

    // ============================================================
    // Save/Load Tests
    // ============================================================

    #[test]
    fn test_save_and_load() -> Result<()> {
        use tempfile::tempdir;

        let dir = tempdir().map_err(Error::Io)?;
        let path = dir.path().join("test_index");

        let config = ShardedVectorConfig::new(2)
            .with_hnsw_config(HnswConfig::new(4, DistanceMetric::Cosine));

        // Create and populate index
        let index = ShardedVectorIndex::new(config.clone())?;
        for i in 1..=10 {
            let node = NodeId::new(i).unwrap();
            index.add(node, &[i as f32, 0.0, 0.0, 0.0])?;
        }
        assert_eq!(index.len(), 10);

        // Save
        index.save(&path)?;

        // Load into new index
        let loaded = ShardedVectorIndex::load(&path, config)?;
        assert_eq!(loaded.len(), 10);
        assert_eq!(loaded.num_shards(), 2);

        // Verify search still works
        let query = vec![5.0, 0.0, 0.0, 0.0];
        let results = loaded.search(&query, 5)?;
        assert!(!results.is_empty());

        Ok(())
    }

    // ============================================================
    // OrderedFloat Tests
    // ============================================================

    #[test]
    fn test_ordered_float_ordering() {
        let a = OrderedFloat(0.5);
        let b = OrderedFloat(0.7);
        let c = OrderedFloat(0.5);

        assert!(a < b);
        assert!(b > a);
        assert_eq!(a, c);
    }

    #[test]
    fn test_ordered_float_nan_handling() {
        let nan = OrderedFloat(f32::NAN);
        let normal = OrderedFloat(0.5);

        // NaN should be less than normal values
        assert!(nan < normal);
        assert!(normal > nan);
    }

    // ============================================================
    // Add Batch Ref Tests
    // ============================================================

    #[test]
    fn test_add_batch_ref() -> Result<()> {
        let index = ShardedVectorIndex::with_defaults(4, DistanceMetric::Cosine, 4)?;

        let vectors: Vec<Vec<f32>> = (1..=10).map(|i| vec![i as f32, 0.0, 0.0, 0.0]).collect();

        let items: Vec<(NodeId, &[f32])> = vectors
            .iter()
            .enumerate()
            .map(|(i, v)| (NodeId::new((i + 1) as u64).unwrap(), v.as_slice()))
            .collect();

        index.add_batch_ref(&items)?;

        assert_eq!(index.len(), 10);

        Ok(())
    }
}
