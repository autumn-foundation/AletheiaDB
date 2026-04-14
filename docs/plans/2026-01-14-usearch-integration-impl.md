# USearch Integration Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Replace hnsw_rs with usearch backend, adding native deletes, quantization (f16/i8), memory-mapped indexes, and custom distance metrics.

**Architecture:** Swap the internal HNSW implementation from hnsw_rs to usearch while preserving the existing `VectorIndex` trait interface. Add new configuration options for quantization and storage modes. Extend the trait with bulk operations, persistence, and stats methods.

**Tech Stack:** Rust, usearch (git fork), DashMap, parking_lot, proptest

---

## Task 1: Update Cargo.toml Dependencies

**Files:**
- Modify: `Cargo.toml`

**Step 1: Remove hnsw_rs dependency and add usearch**

Open `Cargo.toml` and find the line:
```toml
hnsw_rs = "0.3"
```

Replace it with:
```toml
usearch = { git = "https://github.com/madmax983/USearch", branch = "fix/rust-move-semantics" }
```

**Step 2: Verify the change compiles (expect errors)**

Run: `cargo check 2>&1 | head -50`

Expected: Compilation errors about missing `hnsw_rs` imports - this confirms the dependency swap worked.

**Step 3: Commit dependency change**

```bash
git add Cargo.toml Cargo.lock
git commit -m "build: replace hnsw_rs with usearch git dependency"
```

---

## Task 2: Add New Configuration Types

**Files:**
- Modify: `src/index/vector/mod.rs`

**Step 1: Write tests for new types**

Add to `src/index/vector/mod.rs` in the `tests` module:

```rust
#[test]
fn test_quantization_default() {
    assert_eq!(Quantization::default(), Quantization::F32);
}

#[test]
fn test_storage_mode_default() {
    assert!(matches!(StorageMode::default(), StorageMode::InMemory));
}

#[test]
fn test_distance_metric_new_variants() {
    // Test new variants serialize/deserialize correctly
    assert_eq!(DistanceMetric::Haversine.to_u8(), 3);
    assert_eq!(DistanceMetric::Hamming.to_u8(), 4);
    assert_eq!(DistanceMetric::Tanimoto.to_u8(), 5);
    assert_eq!(DistanceMetric::from_u8(3).unwrap(), DistanceMetric::Haversine);
    assert_eq!(DistanceMetric::from_u8(4).unwrap(), DistanceMetric::Hamming);
    assert_eq!(DistanceMetric::from_u8(5).unwrap(), DistanceMetric::Tanimoto);
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test test_quantization_default test_storage_mode_default test_distance_metric_new_variants -- --nocapture 2>&1 | tail -20`

Expected: FAIL - types don't exist yet

**Step 3: Add Quantization enum**

Add before the `DistanceMetric` enum in `src/index/vector/mod.rs`:

```rust
use std::path::PathBuf;
use std::sync::Arc;

/// Quantization level for vector storage.
///
/// Lower precision reduces memory usage but may impact recall slightly.
/// - F32: Full precision (default), no recall impact
/// - F16: Half precision, ~2x memory savings, <1% recall impact typical
/// - I8: Quarter precision, ~4x memory savings, 1-3% recall impact typical
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Quantization {
    /// 32-bit floating point (default, full precision)
    #[default]
    F32,
    /// 16-bit floating point (half precision, ~2x memory savings)
    F16,
    /// 8-bit signed integer (quarter precision, ~4x memory savings)
    I8,
}

/// Storage mode for the vector index.
///
/// - InMemory: All data in RAM (default, fastest queries)
/// - MemoryMapped: Data on disk, lazily loaded (saves RAM, slightly slower)
#[derive(Debug, Clone, Default)]
pub enum StorageMode {
    /// Store index entirely in memory (default)
    #[default]
    InMemory,
    /// Memory-map index from disk path
    MemoryMapped {
        /// Path to the index file
        path: PathBuf,
    },
}

/// Custom distance metric function.
///
/// Allows user-defined similarity functions for specialized use cases.
pub struct CustomMetric {
    /// Human-readable name for the metric
    pub name: String,
    /// The distance function: takes two vectors, returns distance (lower = more similar)
    pub distance_fn: Arc<dyn Fn(&[f32], &[f32]) -> f32 + Send + Sync>,
}

impl std::fmt::Debug for CustomMetric {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CustomMetric")
            .field("name", &self.name)
            .field("distance_fn", &"<function>")
            .finish()
    }
}
```

**Step 4: Extend DistanceMetric enum**

Update the `DistanceMetric` enum to add new variants:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DistanceMetric {
    /// Cosine similarity: measures angle between vectors, range [-1, 1]
    Cosine,
    /// Euclidean distance (L2): measures straight-line distance, range [0, ∞)
    Euclidean,
    /// Dot product: inner product of vectors, range (-∞, ∞)
    DotProduct,
    /// Haversine: great circle distance for geographic coordinates
    Haversine,
    /// Hamming: bit-level distance for binary vectors
    Hamming,
    /// Tanimoto: bit-level Jaccard similarity for chemical fingerprints
    Tanimoto,
}
```

Update `to_u8`:
```rust
pub fn to_u8(self) -> u8 {
    match self {
        DistanceMetric::Cosine => 0,
        DistanceMetric::Euclidean => 1,
        DistanceMetric::DotProduct => 2,
        DistanceMetric::Haversine => 3,
        DistanceMetric::Hamming => 4,
        DistanceMetric::Tanimoto => 5,
    }
}
```

Update `from_u8`:
```rust
pub fn from_u8(value: u8) -> Result<Self> {
    match value {
        0 => Ok(DistanceMetric::Cosine),
        1 => Ok(DistanceMetric::Euclidean),
        2 => Ok(DistanceMetric::DotProduct),
        3 => Ok(DistanceMetric::Haversine),
        4 => Ok(DistanceMetric::Hamming),
        5 => Ok(DistanceMetric::Tanimoto),
        _ => Err(crate::utils::error::StorageError::CorruptedData(format!(
            "Invalid distance metric encoding: {}",
            value
        ))
        .into()),
    }
}
```

**Step 5: Update re-exports**

Add to the re-exports at the bottom of `mod.rs`:
```rust
pub use self::{Quantization, StorageMode, CustomMetric};
```

**Step 6: Run tests to verify they pass**

Run: `cargo test test_quantization_default test_storage_mode_default test_distance_metric_new_variants -- --nocapture`

Expected: PASS

**Step 7: Commit**

```bash
git add src/index/vector/mod.rs
git commit -m "feat(vector): add Quantization, StorageMode, CustomMetric types"
```

---

## Task 3: Extend VectorIndex Trait

**Files:**
- Modify: `src/index/vector/mod.rs`

**Step 1: Add new trait methods with default implementations**

Add these methods to the `VectorIndex` trait:

```rust
/// Adds multiple vectors in a batch operation.
///
/// More efficient than calling `add()` repeatedly for bulk insertions.
/// Default implementation calls `add()` sequentially.
fn add_batch(&self, items: &[(NodeId, Vec<f32>)]) -> Result<()> {
    for (id, vec) in items {
        self.add(*id, vec)?;
    }
    Ok(())
}

/// Removes multiple vectors in a batch operation.
///
/// Default implementation calls `remove()` sequentially.
fn remove_batch(&self, ids: &[NodeId]) -> Result<()> {
    for id in ids {
        self.remove(*id)?;
    }
    Ok(())
}

/// Saves the index to a file path.
///
/// Returns `Err(UnsupportedOperation)` if the implementation doesn't support persistence.
fn save(&self, _path: &std::path::Path) -> Result<()> {
    Err(crate::utils::Error::Vector(
        crate::utils::error::VectorError::IndexError {
            message: "save not supported by this index type".to_string(),
        },
    ))
}

/// Returns the approximate memory usage of this index in bytes.
///
/// Default returns 0 (unknown).
fn memory_usage(&self) -> usize {
    0
}

/// Returns the quantization level of this index.
///
/// Default returns F32 (full precision).
fn quantization(&self) -> Quantization {
    Quantization::F32
}

/// Compacts the index, reclaiming space from deleted entries.
///
/// For indexes that support native deletes, this may be a no-op.
/// For indexes using soft deletes, this rebuilds the index.
fn compact(&self) -> Result<()> {
    Ok(())
}
```

**Step 2: Run cargo check**

Run: `cargo check 2>&1 | head -30`

Expected: Errors about hnsw_rs (expected at this stage)

**Step 3: Commit**

```bash
git add src/index/vector/mod.rs
git commit -m "feat(vector): extend VectorIndex trait with batch ops, persistence, stats"
```

---

## Task 4: Update HnswConfig

**Files:**
- Modify: `src/index/vector/hnsw.rs`

**Step 1: Write test for new config fields**

Add to tests in `hnsw.rs`:

```rust
#[test]
fn test_hnsw_config_new_fields() {
    use crate::index::vector::{Quantization, StorageMode};

    let config = HnswConfig::new(384, DistanceMetric::Cosine)
        .with_quantization(Quantization::F16)
        .with_storage(StorageMode::InMemory);

    assert_eq!(config.quantization, Quantization::F16);
    assert!(matches!(config.storage, StorageMode::InMemory));
}

#[test]
fn test_hnsw_config_custom_metric() {
    use crate::index::vector::CustomMetric;
    use std::sync::Arc;

    let config = HnswConfig::new(4, DistanceMetric::Cosine)
        .with_custom_metric("weighted", |a, b| {
            a.iter().zip(b.iter()).map(|(x, y)| (x - y).abs()).sum()
        });

    assert!(config.custom_metric.is_some());
    assert_eq!(config.custom_metric.as_ref().unwrap().name, "weighted");
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test test_hnsw_config_new_fields test_hnsw_config_custom_metric 2>&1 | tail -20`

Expected: FAIL - methods don't exist yet

**Step 3: Update HnswConfig struct**

Update `HnswConfig` in `src/index/vector/hnsw.rs`:

```rust
use crate::index::vector::{CustomMetric, DistanceMetric, Quantization, StorageMode, VectorIndex};

/// Configuration for HNSW (Hierarchical Navigable Small World) index.
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
```

Note: Remove `PartialEq, Eq` derives since `CustomMetric` contains a function.

**Step 4: Add new builder methods**

Add to `impl HnswConfig`:

```rust
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
        distance_fn: std::sync::Arc::new(f),
    });
    self
}
```

**Step 5: Run tests**

Run: `cargo test test_hnsw_config_new_fields test_hnsw_config_custom_metric 2>&1 | tail -20`

Expected: Still failing due to hnsw_rs import errors

**Step 6: Commit config changes**

```bash
git add src/index/vector/hnsw.rs
git commit -m "feat(vector): extend HnswConfig with quantization, storage, custom metric"
```

---

## Task 5: Rewrite HnswIndex with usearch Backend

**Files:**
- Modify: `src/index/vector/hnsw.rs`

This is the main implementation task. We'll replace the entire internal implementation.

**Step 1: Update imports**

Replace the imports at the top of `hnsw.rs`:

```rust
//! HNSW (Hierarchical Navigable Small World) vector index implementation.
//!
//! This module provides a wrapper around the `usearch` library's HNSW index,
//! implementing the `VectorIndex` trait for approximate k-nearest neighbor search.

use crate::core::id::NodeId;
use crate::core::vector::validate_vector;
use crate::index::vector::{CustomMetric, DistanceMetric, Quantization, StorageMode, VectorIndex};
use crate::utils::{Error, Result, error::VectorError};
use dashmap::DashMap;
use parking_lot::RwLock;
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use usearch::{Index, IndexOptions, MetricKind, ScalarKind};
```

**Step 2: Add helper conversion functions**

Add after imports:

```rust
/// Maximum number of results that can be requested in a search.
const MAX_K: usize = 10_000;

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
```

**Step 3: Rewrite HnswIndex struct**

Replace the `HnswIndex` struct and its internals:

```rust
/// Statistics for index operations.
#[derive(Debug, Default)]
struct IndexStats {
    vectors_added: AtomicU64,
    vectors_removed: AtomicU64,
    searches_performed: AtomicU64,
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
}
```

**Step 4: Implement HnswIndexBuilder**

Rewrite the builder:

```rust
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
        let index = Index::new(&options).map_err(|e| {
            Error::Vector(VectorError::IndexError {
                message: format!("Failed to create usearch index: {}", e),
            })
        })?;

        // Reserve capacity if specified
        if self.config.capacity > 0 {
            index.reserve(self.config.capacity).map_err(|e| {
                Error::Vector(VectorError::IndexError {
                    message: format!("Failed to reserve capacity: {}", e),
                })
            })?;
        }

        // Handle memory-mapped storage
        if let StorageMode::MemoryMapped { ref path } = self.config.storage {
            // Save initial empty index to create the file
            index.save(path.to_str().unwrap_or("index.usearch")).map_err(|e| {
                Error::Vector(VectorError::IndexError {
                    message: format!("Failed to create memory-mapped index: {}", e),
                })
            })?;
            // Switch to view mode (memory-mapped)
            index.view(path.to_str().unwrap_or("index.usearch")).map_err(|e| {
                Error::Vector(VectorError::IndexError {
                    message: format!("Failed to memory-map index: {}", e),
                })
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
        })
    }
}
```

**Step 5: Implement VectorIndex trait**

```rust
impl VectorIndex for HnswIndex {
    fn add(&self, id: NodeId, vector: &[f32]) -> Result<()> {
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
        let key = if let Some(entry) = self.id_mapping.get(&id) {
            // If re-adding, remove old entry first (usearch supports this)
            let existing_key = *entry.value();
            let index = self.inner.write();
            let _ = index.remove(existing_key); // Ignore if not found
            existing_key
        } else {
            let key = self.next_key.fetch_add(1, Ordering::SeqCst);
            self.id_mapping.insert(id, key);
            self.reverse_mapping.insert(key, id);
            key
        };

        // Insert into usearch index
        let index = self.inner.write();
        index.add(key, vector).map_err(|e| {
            Error::Vector(VectorError::IndexError {
                message: format!("Failed to add vector: {}", e),
            })
        })?;

        self.stats.vectors_added.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn remove(&self, id: NodeId) -> Result<()> {
        // Find the key for this NodeId
        if let Some((_, key)) = self.id_mapping.remove(&id) {
            self.reverse_mapping.remove(&key);

            // Native delete in usearch
            let index = self.inner.write();
            index.remove(key).map_err(|e| {
                Error::Vector(VectorError::IndexError {
                    message: format!("Failed to remove vector: {}", e),
                })
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

        // Perform search
        let index = self.inner.read();
        let matches = index.search(query, k_capped).map_err(|e| {
            Error::Vector(VectorError::IndexError {
                message: format!("Search failed: {}", e),
            })
        })?;

        self.stats.searches_performed.fetch_add(1, Ordering::Relaxed);

        // Convert results to (NodeId, similarity) format
        let mut results: Vec<(NodeId, f32)> = Vec::with_capacity(matches.keys.len());

        for (key, distance) in matches.keys.iter().zip(matches.distances.iter()) {
            if let Some(node_id_ref) = self.reverse_mapping.get(key) {
                let node_id = *node_id_ref.value();

                // Convert distance to similarity based on metric
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

        // Results should already be sorted by usearch, but ensure descending order
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        Ok(results)
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

        // Use usearch's native filtered search
        let index = self.inner.read();

        // Create a filter that maps usearch keys to our predicate
        let filter = |key: u64| -> bool {
            if let Some(node_id_ref) = self.reverse_mapping.get(&key) {
                predicate(node_id_ref.value())
            } else {
                false
            }
        };

        let matches = index.filtered_search(query, k_capped, filter).map_err(|e| {
            Error::Vector(VectorError::IndexError {
                message: format!("Filtered search failed: {}", e),
            })
        })?;

        self.stats.searches_performed.fetch_add(1, Ordering::Relaxed);

        // Convert results
        let mut results: Vec<(NodeId, f32)> = Vec::with_capacity(matches.keys.len());

        for (key, distance) in matches.keys.iter().zip(matches.distances.iter()) {
            if let Some(node_id_ref) = self.reverse_mapping.get(key) {
                let node_id = *node_id_ref.value();
                let similarity = match self.config.metric {
                    DistanceMetric::Cosine => 1.0 - distance,
                    DistanceMetric::Euclidean => -distance,
                    DistanceMetric::DotProduct => -distance,
                    _ => -distance,
                };
                results.push((node_id, similarity));
            }
        }

        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        Ok(results)
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
        let index = self.inner.read();
        index.save(path.to_str().unwrap_or("index.usearch")).map_err(|e| {
            Error::Vector(VectorError::IndexError {
                message: format!("Failed to save index: {}", e),
            })
        })
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
```

**Step 6: Add HnswIndex helper methods**

```rust
impl HnswIndex {
    /// Creates a new HNSW index from a configuration.
    pub fn new(config: HnswConfig) -> Result<Self> {
        HnswIndexBuilder::from_config(&config).build()
    }

    /// Sets the ef_search parameter for query-time search quality.
    pub fn set_ef_search(&self, ef_search: usize) {
        self.inner.read().change_expansion_search(ef_search);
    }

    /// Gets the current ef_search value.
    pub fn get_ef_search(&self) -> usize {
        self.config.ef_search
    }

    /// Returns the configuration used to create this index.
    pub fn config(&self) -> HnswConfig {
        self.config.clone()
    }

    /// Returns the M parameter (connections per node).
    pub fn m(&self) -> usize {
        self.config.m
    }

    /// Loads an index from a file path.
    pub fn load(path: &Path, config: HnswConfig) -> Result<Self> {
        let options = IndexOptions {
            dimensions: config.dimensions,
            metric: to_usearch_metric(config.metric),
            quantization: to_usearch_scalar(config.quantization),
            connectivity: config.m,
            expansion_add: config.ef_construction,
            expansion_search: config.ef_search,
            multi: false,
        };

        let index = Index::new(&options).map_err(|e| {
            Error::Vector(VectorError::IndexError {
                message: format!("Failed to create index for loading: {}", e),
            })
        })?;

        index.load(path.to_str().unwrap_or("index.usearch")).map_err(|e| {
            Error::Vector(VectorError::IndexError {
                message: format!("Failed to load index: {}", e),
            })
        })?;

        Ok(HnswIndex {
            inner: Arc::new(RwLock::new(index)),
            config,
            id_mapping: Arc::new(DashMap::new()),
            reverse_mapping: Arc::new(DashMap::new()),
            next_key: AtomicU64::new(0),
            stats: Arc::new(IndexStats::default()),
            max_k: MAX_K,
        })
    }

    /// Opens a memory-mapped index from a file path.
    pub fn open_mmap(path: &Path) -> Result<Self> {
        let index = Index::new(&IndexOptions::default()).map_err(|e| {
            Error::Vector(VectorError::IndexError {
                message: format!("Failed to create index: {}", e),
            })
        })?;

        index.view(path.to_str().unwrap_or("index.usearch")).map_err(|e| {
            Error::Vector(VectorError::IndexError {
                message: format!("Failed to memory-map index: {}", e),
            })
        })?;

        let dimensions = index.dimensions();
        let connectivity = index.connectivity();

        Ok(HnswIndex {
            inner: Arc::new(RwLock::new(index)),
            config: HnswConfig {
                dimensions,
                m: connectivity,
                storage: StorageMode::MemoryMapped { path: path.to_path_buf() },
                ..Default::default()
            },
            id_mapping: Arc::new(DashMap::new()),
            reverse_mapping: Arc::new(DashMap::new()),
            next_key: AtomicU64::new(0),
            stats: Arc::new(IndexStats::default()),
            max_k: MAX_K,
        })
    }
}

// SAFETY: HnswIndex is safe to send across threads because:
// 1. usearch::Index is thread-safe for concurrent operations
// 2. All our fields use thread-safe wrappers (Arc, RwLock, DashMap, atomics)
unsafe impl Send for HnswIndex {}
unsafe impl Sync for HnswIndex {}
```

**Step 7: Run cargo check**

Run: `cargo check 2>&1 | head -50`

Expected: Should compile (or have minor fixable errors)

**Step 8: Run existing tests**

Run: `cargo test --lib hnsw -- --nocapture 2>&1 | tail -50`

Expected: Tests should pass

**Step 9: Commit**

```bash
git add src/index/vector/hnsw.rs
git commit -m "feat(vector): rewrite HnswIndex with usearch backend

- Native delete support (no more soft deletes)
- f16/i8 quantization support
- Memory-mapped index support
- All new distance metrics (Haversine, Hamming, Tanimoto)
- Bulk add/remove operations
- Index persistence (save/load)"
```

---

## Task 6: Update Temporal Vector Index

**Files:**
- Modify: `src/index/vector/temporal.rs`

**Step 1: Check for any hnsw_rs-specific code**

Run: `grep -n "hnsw_rs\|HnswIndexInner\|deleted_ids\|vector_cache" src/index/vector/temporal.rs`

Expected: Identify any code that needs updating

**Step 2: Update imports if needed**

The temporal index uses `HnswIndex` via the `VectorIndex` trait, so it should work with minimal changes. Update any imports that reference removed types.

**Step 3: Run temporal tests**

Run: `cargo test --lib temporal -- --nocapture 2>&1 | tail -50`

Expected: Tests should pass

**Step 4: Commit if changes were needed**

```bash
git add src/index/vector/temporal.rs
git commit -m "refactor(vector): update temporal index for usearch backend"
```

---

## Task 7: Add Native Delete Tests

**Files:**
- Create: `tests/vector_native_delete_tests.rs`

**Step 1: Create the test file**

```rust
//! Tests verifying usearch native delete functionality.

use aletheiadb::core::id::NodeId;
use aletheiadb::index::vector::{DistanceMetric, HnswIndexBuilder, VectorIndex};

/// Test that native deletes truly remove vectors from the index.
#[test]
fn test_native_delete_removes_from_index() {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();

    let node1 = NodeId::new(1).unwrap();
    let node2 = NodeId::new(2).unwrap();
    let node3 = NodeId::new(3).unwrap();

    // Add three vectors
    index.add(node1, &[1.0, 0.0, 0.0, 0.0]).unwrap();
    index.add(node2, &[0.0, 1.0, 0.0, 0.0]).unwrap();
    index.add(node3, &[0.0, 0.0, 1.0, 0.0]).unwrap();

    assert_eq!(index.len(), 3);

    // Delete node2
    index.remove(node2).unwrap();

    // Verify length decreased
    assert_eq!(index.len(), 2);

    // Search for vectors similar to node2's original position
    let results = index.search(&[0.0, 1.0, 0.0, 0.0], 10).unwrap();

    // node2 should NOT appear in results (native delete, not soft delete)
    for (id, _) in &results {
        assert_ne!(*id, node2, "Deleted node should not appear in search results");
    }
}

/// Test that re-adding a deleted node works correctly.
#[test]
fn test_readd_after_delete() {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();

    let node1 = NodeId::new(1).unwrap();

    // Add, delete, re-add with different vector
    index.add(node1, &[1.0, 0.0, 0.0, 0.0]).unwrap();
    index.remove(node1).unwrap();
    index.add(node1, &[0.0, 0.0, 0.0, 1.0]).unwrap();

    assert_eq!(index.len(), 1);

    // Search should find the new vector
    let results = index.search(&[0.0, 0.0, 0.0, 1.0], 1).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, node1);
    assert!(results[0].1 > 0.99); // Should be very similar
}

/// Test batch delete.
#[test]
fn test_batch_delete() {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();

    // Add 100 nodes
    for i in 1..=100 {
        let node = NodeId::new(i).unwrap();
        index.add(node, &[i as f32, 0.0, 0.0, 0.0]).unwrap();
    }
    assert_eq!(index.len(), 100);

    // Delete nodes 1-50
    let to_delete: Vec<NodeId> = (1..=50).map(|i| NodeId::new(i).unwrap()).collect();
    index.remove_batch(&to_delete).unwrap();

    // Verify length
    assert_eq!(index.len(), 50);

    // Verify deleted nodes don't appear in search
    let results = index.search(&[25.0, 0.0, 0.0, 0.0], 100).unwrap();
    for (id, _) in &results {
        assert!(id.as_u64() > 50, "Deleted node {} found in results", id.as_u64());
    }
}
```

**Step 2: Run the tests**

Run: `cargo test --test vector_native_delete_tests -- --nocapture`

Expected: PASS

**Step 3: Commit**

```bash
git add tests/vector_native_delete_tests.rs
git commit -m "test(vector): add native delete verification tests"
```

---

## Task 8: Add Quantization Tests

**Files:**
- Create: `tests/vector_quantization_tests.rs`

**Step 1: Create the test file**

```rust
//! Tests verifying quantization correctness and recall.

use aletheiadb::core::id::NodeId;
use aletheiadb::index::vector::{DistanceMetric, HnswIndexBuilder, Quantization, VectorIndex};
use std::collections::HashSet;

/// Helper to generate random-ish vectors for testing.
fn generate_vectors(count: usize, dims: usize) -> Vec<Vec<f32>> {
    (0..count)
        .map(|i| {
            (0..dims)
                .map(|j| ((i * 17 + j * 31) % 1000) as f32 / 1000.0)
                .collect()
        })
        .collect()
}

/// Calculate recall: what fraction of f32 results appear in quantized results.
fn calculate_recall(baseline: &[(NodeId, f32)], test: &[(NodeId, f32)]) -> f64 {
    let baseline_ids: HashSet<_> = baseline.iter().map(|(id, _)| *id).collect();
    let test_ids: HashSet<_> = test.iter().map(|(id, _)| *id).collect();

    let intersection = baseline_ids.intersection(&test_ids).count();
    if baseline_ids.is_empty() {
        1.0
    } else {
        intersection as f64 / baseline_ids.len() as f64
    }
}

/// Test f16 quantization recall >= 95%.
#[test]
fn test_f16_quantization_recall() {
    let dims = 128;
    let vectors = generate_vectors(1000, dims);

    // Build f32 baseline index
    let f32_index = HnswIndexBuilder::new(dims, DistanceMetric::Cosine)
        .ef_construction(200)
        .ef_search(100)
        .build()
        .unwrap();

    // Build f16 index
    let f16_index = HnswIndexBuilder::new(dims, DistanceMetric::Cosine)
        .ef_construction(200)
        .ef_search(100)
        .quantization(Quantization::F16)
        .build()
        .unwrap();

    // Add vectors to both
    for (i, vec) in vectors.iter().enumerate() {
        let node = NodeId::new(i as u64 + 1).unwrap();
        f32_index.add(node, vec).unwrap();
        f16_index.add(node, vec).unwrap();
    }

    // Test recall across multiple queries
    let mut total_recall = 0.0;
    let num_queries = 10;

    for i in 0..num_queries {
        let query = &vectors[i * 100]; // Use existing vectors as queries

        let f32_results = f32_index.search(query, 10).unwrap();
        let f16_results = f16_index.search(query, 10).unwrap();

        total_recall += calculate_recall(&f32_results, &f16_results);
    }

    let avg_recall = total_recall / num_queries as f64;
    assert!(
        avg_recall >= 0.95,
        "F16 recall {:.2}% is below 95% threshold",
        avg_recall * 100.0
    );

    // Verify memory savings
    let f32_memory = f32_index.memory_usage();
    let f16_memory = f16_index.memory_usage();

    // F16 should use roughly half the memory (allow some overhead)
    if f32_memory > 0 && f16_memory > 0 {
        let ratio = f32_memory as f64 / f16_memory as f64;
        assert!(
            ratio > 1.5,
            "F16 memory ratio {:.2}x is less than expected 1.5x savings",
            ratio
        );
    }
}

/// Test i8 quantization recall >= 90%.
#[test]
fn test_i8_quantization_recall() {
    let dims = 128;
    let vectors = generate_vectors(1000, dims);

    let f32_index = HnswIndexBuilder::new(dims, DistanceMetric::Cosine)
        .ef_construction(200)
        .ef_search(100)
        .build()
        .unwrap();

    let i8_index = HnswIndexBuilder::new(dims, DistanceMetric::Cosine)
        .ef_construction(200)
        .ef_search(100)
        .quantization(Quantization::I8)
        .build()
        .unwrap();

    for (i, vec) in vectors.iter().enumerate() {
        let node = NodeId::new(i as u64 + 1).unwrap();
        f32_index.add(node, vec).unwrap();
        i8_index.add(node, vec).unwrap();
    }

    let mut total_recall = 0.0;
    let num_queries = 10;

    for i in 0..num_queries {
        let query = &vectors[i * 100];

        let f32_results = f32_index.search(query, 10).unwrap();
        let i8_results = i8_index.search(query, 10).unwrap();

        total_recall += calculate_recall(&f32_results, &i8_results);
    }

    let avg_recall = total_recall / num_queries as f64;
    assert!(
        avg_recall >= 0.90,
        "I8 recall {:.2}% is below 90% threshold",
        avg_recall * 100.0
    );
}

/// Test that quantization setting is preserved.
#[test]
fn test_quantization_preserved() {
    let f16_index = HnswIndexBuilder::new(64, DistanceMetric::Cosine)
        .quantization(Quantization::F16)
        .build()
        .unwrap();

    assert_eq!(f16_index.quantization(), Quantization::F16);

    let i8_index = HnswIndexBuilder::new(64, DistanceMetric::Cosine)
        .quantization(Quantization::I8)
        .build()
        .unwrap();

    assert_eq!(i8_index.quantization(), Quantization::I8);
}
```

**Step 2: Run the tests**

Run: `cargo test --test vector_quantization_tests -- --nocapture`

Expected: PASS

**Step 3: Commit**

```bash
git add tests/vector_quantization_tests.rs
git commit -m "test(vector): add quantization recall verification tests"
```

---

## Task 9: Add Memory-Mapped Index Tests

**Files:**
- Create: `tests/vector_mmap_tests.rs`

**Step 1: Create the test file**

```rust
//! Tests for memory-mapped index persistence.

use aletheiadb::core::id::NodeId;
use aletheiadb::index::vector::{DistanceMetric, HnswConfig, HnswIndex, HnswIndexBuilder, StorageMode, VectorIndex};
use std::path::PathBuf;
use tempfile::TempDir;

/// Test save and load roundtrip.
#[test]
fn test_save_load_roundtrip() {
    let temp_dir = TempDir::new().unwrap();
    let index_path = temp_dir.path().join("test_index.usearch");

    // Create and populate index
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();

    let node1 = NodeId::new(1).unwrap();
    let node2 = NodeId::new(2).unwrap();

    index.add(node1, &[1.0, 0.0, 0.0, 0.0]).unwrap();
    index.add(node2, &[0.0, 1.0, 0.0, 0.0]).unwrap();

    // Save
    index.save(&index_path).unwrap();

    // Load into new index
    let config = HnswConfig::new(4, DistanceMetric::Cosine);
    let loaded = HnswIndex::load(&index_path, config).unwrap();

    // Verify data
    assert_eq!(loaded.len(), 2);

    let results = loaded.search(&[1.0, 0.0, 0.0, 0.0], 2).unwrap();
    assert!(!results.is_empty());
}

/// Test memory-mapped index creation and query.
#[test]
fn test_mmap_index_query() {
    let temp_dir = TempDir::new().unwrap();
    let index_path = temp_dir.path().join("mmap_index.usearch");

    // Create memory-mapped index
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .storage(StorageMode::MemoryMapped { path: index_path.clone() })
        .build()
        .unwrap();

    // Add vectors
    for i in 1..=100 {
        let node = NodeId::new(i).unwrap();
        let vec = vec![i as f32, 0.0, 0.0, 0.0];
        index.add(node, &vec).unwrap();
    }

    // Query
    let results = index.search(&[50.0, 0.0, 0.0, 0.0], 5).unwrap();
    assert_eq!(results.len(), 5);

    // Verify file exists
    assert!(index_path.exists());
}

/// Test opening existing memory-mapped index.
#[test]
fn test_open_mmap_index() {
    let temp_dir = TempDir::new().unwrap();
    let index_path = temp_dir.path().join("existing_mmap.usearch");

    // Create and save index
    {
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap();

        index.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0]).unwrap();
        index.add(NodeId::new(2).unwrap(), &[0.0, 1.0, 0.0, 0.0]).unwrap();

        index.save(&index_path).unwrap();
    }

    // Open as memory-mapped
    let mmap_index = HnswIndex::open_mmap(&index_path).unwrap();

    // Should be able to query
    let results = mmap_index.search(&[1.0, 0.0, 0.0, 0.0], 2).unwrap();
    assert!(!results.is_empty());
}
```

**Step 2: Add tempfile dev dependency**

Add to `Cargo.toml` under `[dev-dependencies]`:
```toml
tempfile = "3.10"
```

**Step 3: Run the tests**

Run: `cargo test --test vector_mmap_tests -- --nocapture`

Expected: PASS

**Step 4: Commit**

```bash
git add tests/vector_mmap_tests.rs Cargo.toml
git commit -m "test(vector): add memory-mapped index persistence tests"
```

---

## Task 10: Add Property-Based Tests

**Files:**
- Create: `tests/vector_proptest.rs`

**Step 1: Create the test file**

```rust
//! Property-based tests for vector index invariants.

use aletheiadb::core::id::NodeId;
use aletheiadb::index::vector::{DistanceMetric, HnswIndexBuilder, VectorIndex};
use proptest::prelude::*;

/// Generate valid vector dimensions (4-128 for test speed).
fn dims_strategy() -> impl Strategy<Value = usize> {
    4usize..=128
}

/// Generate valid vector with given dimensions.
fn vector_strategy(dims: usize) -> impl Strategy<Value = Vec<f32>> {
    proptest::collection::vec(-1.0f32..=1.0, dims)
}

/// Generate valid NodeId.
fn node_id_strategy() -> impl Strategy<Value = NodeId> {
    (1u64..10000).prop_map(|id| NodeId::new(id).unwrap())
}

proptest! {
    /// Invariant: search results are always sorted by similarity (descending).
    #[test]
    fn prop_results_sorted(
        dims in dims_strategy(),
        vectors in proptest::collection::vec(vector_strategy(128), 10..50),
        query in vector_strategy(128),
        k in 1usize..20
    ) {
        // Use fixed dims for this test
        let dims = 128;
        let index = HnswIndexBuilder::new(dims, DistanceMetric::Cosine)
            .build()
            .unwrap();

        for (i, vec) in vectors.iter().enumerate() {
            let node = NodeId::new(i as u64 + 1).unwrap();
            index.add(node, vec).unwrap();
        }

        let results = index.search(&query, k).unwrap();

        // Verify sorted descending
        for i in 1..results.len() {
            prop_assert!(
                results[i-1].1 >= results[i].1,
                "Results not sorted: {} > {} at positions {}, {}",
                results[i].1, results[i-1].1, i-1, i
            );
        }
    }

    /// Invariant: delete followed by search never returns deleted ID.
    #[test]
    fn prop_delete_removes_from_results(
        vectors in proptest::collection::vec(vector_strategy(64), 20..100),
        delete_indices in proptest::collection::vec(0usize..20, 1..10),
        query in vector_strategy(64)
    ) {
        let dims = 64;
        let index = HnswIndexBuilder::new(dims, DistanceMetric::Cosine)
            .build()
            .unwrap();

        // Add all vectors
        let mut node_ids: Vec<NodeId> = Vec::new();
        for (i, vec) in vectors.iter().enumerate() {
            let node = NodeId::new(i as u64 + 1).unwrap();
            node_ids.push(node);
            index.add(node, vec).unwrap();
        }

        // Delete some
        let mut deleted: std::collections::HashSet<NodeId> = std::collections::HashSet::new();
        for idx in delete_indices {
            let idx = idx % node_ids.len();
            let node = node_ids[idx];
            if !deleted.contains(&node) {
                index.remove(node).unwrap();
                deleted.insert(node);
            }
        }

        // Search should never return deleted IDs
        let results = index.search(&query, 100).unwrap();

        for (id, _) in results {
            prop_assert!(
                !deleted.contains(&id),
                "Deleted node {:?} found in search results",
                id
            );
        }
    }

    /// Invariant: len() equals number of adds minus number of removes.
    #[test]
    fn prop_len_tracks_operations(
        add_count in 10usize..100,
        remove_indices in proptest::collection::vec(0usize..10, 0..5)
    ) {
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap();

        // Add vectors
        for i in 0..add_count {
            let node = NodeId::new(i as u64 + 1).unwrap();
            index.add(node, &[i as f32, 0.0, 0.0, 0.0]).unwrap();
        }

        prop_assert_eq!(index.len(), add_count);

        // Remove some (avoiding duplicates)
        let mut removed = 0;
        let mut removed_set: std::collections::HashSet<usize> = std::collections::HashSet::new();
        for idx in remove_indices {
            let idx = idx % add_count;
            if !removed_set.contains(&idx) {
                let node = NodeId::new(idx as u64 + 1).unwrap();
                index.remove(node).unwrap();
                removed += 1;
                removed_set.insert(idx);
            }
        }

        prop_assert_eq!(index.len(), add_count - removed);
    }
}
```

**Step 2: Run the tests**

Run: `cargo test --test vector_proptest -- --nocapture`

Expected: PASS

**Step 3: Commit**

```bash
git add tests/vector_proptest.rs
git commit -m "test(vector): add property-based tests for index invariants"
```

---

## Task 11: Add Stress Tests

**Files:**
- Create: `tests/vector_stress_tests.rs`

**Step 1: Create the test file**

```rust
//! Stress tests for concurrent vector index operations.

use aletheiadb::core::id::NodeId;
use aletheiadb::index::vector::{DistanceMetric, HnswIndexBuilder, VectorIndex};
use std::sync::Arc;
use std::thread;

/// Stress test: concurrent add/search/delete operations.
#[test]
fn stress_concurrent_operations() {
    let index = Arc::new(
        HnswIndexBuilder::new(64, DistanceMetric::Cosine)
            .initial_capacity(10000)
            .build()
            .unwrap()
    );

    let num_threads = 8;
    let ops_per_thread = 1000;

    let mut handles = vec![];

    for thread_id in 0..num_threads {
        let index = Arc::clone(&index);

        let handle = thread::spawn(move || {
            let base_id = thread_id * ops_per_thread;

            for i in 0..ops_per_thread {
                let node_id = NodeId::new((base_id + i) as u64 + 1).unwrap();
                let vector: Vec<f32> = (0..64).map(|j| (i + j) as f32 / 1000.0).collect();

                // Add
                index.add(node_id, &vector).unwrap();

                // Search (every 10th operation)
                if i % 10 == 0 {
                    let query: Vec<f32> = (0..64).map(|j| (i + j + 1) as f32 / 1000.0).collect();
                    let _ = index.search(&query, 10);
                }

                // Delete (every 5th operation)
                if i % 5 == 0 && i > 0 {
                    let delete_id = NodeId::new((base_id + i - 1) as u64 + 1).unwrap();
                    let _ = index.remove(delete_id);
                }
            }
        });

        handles.push(handle);
    }

    // Wait for all threads
    for handle in handles {
        handle.join().expect("Thread panicked");
    }

    // Verify index is still usable
    let results = index.search(&vec![0.5f32; 64], 10);
    assert!(results.is_ok());
}

/// Stress test: rapid add/remove cycles.
#[test]
fn stress_rapid_add_remove() {
    let index = HnswIndexBuilder::new(32, DistanceMetric::Cosine)
        .build()
        .unwrap();

    let node = NodeId::new(1).unwrap();
    let vector = vec![0.5f32; 32];

    // Rapid add/remove cycles
    for _ in 0..1000 {
        index.add(node, &vector).unwrap();
        index.remove(node).unwrap();
    }

    // Final state should be empty
    assert_eq!(index.len(), 0);

    // Should be able to add again
    index.add(node, &vector).unwrap();
    assert_eq!(index.len(), 1);
}

/// Stress test: many searches on large index.
#[test]
fn stress_search_throughput() {
    let index = HnswIndexBuilder::new(128, DistanceMetric::Cosine)
        .initial_capacity(10000)
        .build()
        .unwrap();

    // Build index with 10k vectors
    for i in 0..10000 {
        let node = NodeId::new(i as u64 + 1).unwrap();
        let vector: Vec<f32> = (0..128).map(|j| ((i * 17 + j * 31) % 1000) as f32 / 1000.0).collect();
        index.add(node, &vector).unwrap();
    }

    // Perform 1000 searches
    let start = std::time::Instant::now();

    for i in 0..1000 {
        let query: Vec<f32> = (0..128).map(|j| ((i * 13 + j * 29) % 1000) as f32 / 1000.0).collect();
        let results = index.search(&query, 10).unwrap();
        assert!(!results.is_empty());
    }

    let elapsed = start.elapsed();
    let qps = 1000.0 / elapsed.as_secs_f64();

    println!("Search throughput: {:.0} queries/second", qps);

    // Should achieve at least 100 QPS
    assert!(qps > 100.0, "Search throughput {:.0} QPS is too low", qps);
}
```

**Step 2: Run the tests**

Run: `cargo test --test vector_stress_tests -- --nocapture --test-threads=1`

Note: `--test-threads=1` ensures stress tests don't interfere with each other.

Expected: PASS

**Step 3: Commit**

```bash
git add tests/vector_stress_tests.rs
git commit -m "test(vector): add stress tests for concurrent operations"
```

---

## Task 12: Run Full Test Suite and Benchmarks

**Files:**
- None (validation only)

**Step 1: Run clippy**

Run: `cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -30`

Expected: No warnings/errors

**Step 2: Run fmt check**

Run: `cargo fmt --all -- --check`

Expected: No formatting issues (or run `cargo fmt --all` to fix)

**Step 3: Run all tests**

Run: `cargo test --all-features 2>&1 | tail -50`

Expected: All tests pass

**Step 4: Run benchmarks**

Run: `cargo bench --bench hnsw_index 2>&1 | tail -50`

Expected: Benchmarks complete, document results

**Step 5: Commit any final fixes**

```bash
git add -A
git commit -m "chore: fix clippy warnings and formatting"
```

---

## Task 13: Create Pull Request

**Step 1: Push branch**

Run: `git push -u origin feature/usearch-integration`

**Step 2: Create PR**

Run:
```bash
just worktree-pr "feat(vector): replace hnsw_rs with usearch backend" "## Summary

Replaces the pure-Rust hnsw_rs HNSW implementation with usearch (C++ with Rust bindings via fork that fixes move semantics).

### Changes

- **Native deletes**: Removed soft-delete workaround, vectors are truly removed
- **Quantization**: Added F16 and I8 support for 2-4x memory savings
- **Memory-mapped indexes**: Serve large indexes from disk
- **New distance metrics**: Haversine, Hamming, Tanimoto
- **Custom metrics**: User-defined distance functions
- **Bulk operations**: add_batch/remove_batch for efficient bulk loading
- **Persistence**: save/load for index serialization

### Testing

- [x] Native delete tests verify true removal
- [x] Quantization tests verify >95% recall (F16) and >90% recall (I8)
- [x] Memory-mapped tests verify persistence roundtrip
- [x] Property-based tests verify invariants
- [x] Stress tests verify concurrent safety
- [x] All existing tests pass

### Dependency

Uses fork: https://github.com/madmax983/USearch branch fix/rust-move-semantics
Will switch to upstream once PR is merged."
```

---

## Summary

**Total Tasks:** 13

**Files Created:**
- `tests/vector_native_delete_tests.rs`
- `tests/vector_quantization_tests.rs`
- `tests/vector_mmap_tests.rs`
- `tests/vector_proptest.rs`
- `tests/vector_stress_tests.rs`

**Files Modified:**
- `Cargo.toml`
- `src/index/vector/mod.rs`
- `src/index/vector/hnsw.rs`
- `src/index/vector/temporal.rs` (if needed)

**Commits:** ~13 atomic commits following conventional commit format

**Estimated Code Changes:** ~1500 lines added/modified
