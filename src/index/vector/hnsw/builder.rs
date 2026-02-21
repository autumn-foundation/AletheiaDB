//! HNSW index builder.

use crate::core::error::{Error, Result, VectorError};
use crate::core::property::MAX_VECTOR_DIMENSIONS;
use crate::index::vector::{DistanceMetric, Quantization, StorageMode};
use dashmap::DashMap;
use parking_lot::{Mutex, RwLock};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use usearch::{Index, IndexOptions};

use super::config::HnswConfig;
use super::index::{
    HnswIndex, IndexStats, MAX_K, NUM_ENTRY_LOCKS, create_metric_wrapper, to_usearch_metric,
    to_usearch_scalar,
};

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
