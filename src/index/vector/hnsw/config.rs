use super::utils::{create_metric_wrapper, to_usearch_metric, to_usearch_scalar};
use super::{HnswIndex, IndexStats};
use crate::core::error::{Error, Result, VectorError};
use crate::core::property::MAX_VECTOR_DIMENSIONS;
use crate::index::vector::{CustomMetric, DistanceMetric, Quantization, StorageMode};
use dashmap::DashMap;
use parking_lot::{Mutex, RwLock};
use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use usearch::{Index, IndexOptions};

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

/// Builder for configuring and creating an `HnswIndex`.
pub struct HnswIndexBuilder {
    pub(crate) config: HnswConfig,
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
            max_k: super::MAX_K,
            is_mmap: false,
            save_lock: Arc::new(RwLock::new(())),
            entry_locks: (0..super::NUM_ENTRY_LOCKS)
                .map(|_| Mutex::new(()))
                .collect(),
        })
    }
}
