//! HNSW index configuration.

use crate::core::error::{Error, Result, VectorError};
use crate::core::property::MAX_VECTOR_DIMENSIONS;
use crate::index::vector::{CustomMetric, DistanceMetric, Quantization, StorageMode};
use std::io::{Read, Write};
use std::sync::Arc;

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

/// Metadata stored in the mappings file (Version 2+)
pub(crate) struct IndexMetadata {
    pub(crate) dimensions: usize,
    pub(crate) quantization: Quantization,
    pub(crate) metric: DistanceMetric,
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_hnsw_config_serialization_round_trip() {
        let config = HnswConfig {
            dimensions: 128,
            metric: DistanceMetric::Euclidean,
            m: 32,
            ef_construction: 200,
            ef_search: 100,
            capacity: 5000,
            quantization: Quantization::F16,
            storage: StorageMode::InMemory,
            custom_metric: None,
        };

        let mut buffer = Vec::new();
        config.serialize_into(&mut buffer).unwrap();

        let mut cursor = std::io::Cursor::new(buffer);
        let deserialized = HnswConfig::deserialize_from(&mut cursor).unwrap();

        assert_eq!(config, deserialized);
    }

    #[test]
    fn test_hnsw_config_deserialize_legacy() {
        // Legacy format: missing quantization byte
        let config = HnswConfig {
            dimensions: 128,
            metric: DistanceMetric::Cosine,
            m: 16,
            ef_construction: 128,
            ef_search: 64,
            capacity: 1000,
            quantization: Quantization::F32, // Default
            storage: StorageMode::InMemory,
            custom_metric: None,
        };

        let mut buffer = Vec::new();
        // Manually write legacy format
        buffer.extend_from_slice(&(config.dimensions as u64).to_le_bytes());
        buffer.push(config.metric.to_u8());
        buffer.extend_from_slice(&(config.m as u64).to_le_bytes());
        buffer.extend_from_slice(&(config.ef_construction as u64).to_le_bytes());
        buffer.extend_from_slice(&(config.ef_search as u64).to_le_bytes());
        buffer.extend_from_slice(&(config.capacity as u64).to_le_bytes());
        // STOP here (no quantization byte)

        let mut cursor = std::io::Cursor::new(buffer);
        let deserialized = HnswConfig::deserialize_from(&mut cursor).unwrap();

        assert_eq!(config, deserialized);
        assert_eq!(deserialized.quantization, Quantization::F32); // Check default
    }

    #[test]
    fn test_hnsw_config_deserialize_invalid_metric() {
        let mut buffer = Vec::new();
        buffer.extend_from_slice(&128u64.to_le_bytes()); // dimensions
        buffer.push(99); // Invalid metric
        // rest doesn't matter much as it should fail early, but let's pad it
        buffer.resize(100, 0);

        let mut cursor = std::io::Cursor::new(buffer);
        let result = HnswConfig::deserialize_from(&mut cursor);
        assert!(result.is_err());
    }

    #[test]
    fn test_hnsw_config_deserialize_invalid_quantization() {
        // Construct a buffer that is valid until quantization byte
        let config = HnswConfig::default();
        let mut buffer = Vec::new();
        // Write valid parts manually to ensure we reach quantization read
        buffer.extend_from_slice(&(config.dimensions as u64).to_le_bytes());
        buffer.push(config.metric.to_u8());
        buffer.extend_from_slice(&(config.m as u64).to_le_bytes());
        buffer.extend_from_slice(&(config.ef_construction as u64).to_le_bytes());
        buffer.extend_from_slice(&(config.ef_search as u64).to_le_bytes());
        buffer.extend_from_slice(&(config.capacity as u64).to_le_bytes());

        // Write INVALID quantization byte
        buffer.push(99);

        let mut cursor = std::io::Cursor::new(buffer);
        let result = HnswConfig::deserialize_from(&mut cursor);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Invalid quantization")
        );
    }

    #[test]
    fn test_config_deserialize_dimensions_too_large() {
        let huge_dims = (MAX_VECTOR_DIMENSIONS + 1) as u64;
        let mut buffer = Vec::new();
        buffer.extend_from_slice(&huge_dims.to_le_bytes()); // dimensions
        // Add minimal remaining fields to avoid early EOF if we got past dimensions check
        buffer.push(0); // metric
        buffer.extend_from_slice(&16u64.to_le_bytes()); // m
        buffer.extend_from_slice(&128u64.to_le_bytes()); // ef_construction
        buffer.extend_from_slice(&64u64.to_le_bytes()); // ef_search
        buffer.extend_from_slice(&1000u64.to_le_bytes()); // capacity
        buffer.push(0); // quantization

        let mut cursor = std::io::Cursor::new(buffer);
        let result = HnswConfig::deserialize_from(&mut cursor);

        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("dimensions"));
        assert!(msg.contains("exceeds maximum allowed"));
    }

    struct MockReadError;
    impl Read for MockReadError {
        fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("Mock read error"))
        }
    }

    #[test]
    fn test_deserialize_from_read_error() {
        let mut reader = MockReadError;
        let result = HnswConfig::deserialize_from(&mut reader);
        assert!(result.is_err());
    }

    struct MockFailReader {
        data: Vec<u8>,
        fail_at: usize,
        cursor: usize,
    }

    impl MockFailReader {
        fn new(data: Vec<u8>, fail_at: usize) -> Self {
            Self {
                data,
                fail_at,
                cursor: 0,
            }
        }
    }

    impl Read for MockFailReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.cursor >= self.fail_at {
                return Err(std::io::Error::other("Mock read error"));
            }
            // Read as much as possible up to fail_at
            let remaining_before_fail = self.fail_at - self.cursor;
            let available_data = self.data.len() - self.cursor;
            let to_read = std::cmp::min(buf.len(), remaining_before_fail);
            let to_read = std::cmp::min(to_read, available_data);

            if to_read == 0 {
                return Ok(0);
            }

            // Fix: buffer might be larger than data source, so verify bounds
            buf[..to_read].copy_from_slice(&self.data[self.cursor..self.cursor + to_read]);
            self.cursor += to_read;
            Ok(to_read)
        }
    }

    #[test]
    fn test_deserialize_quantization_error() {
        // Construct valid data up to quantization
        let config = HnswConfig::default();
        let mut buffer = Vec::new();
        buffer.extend_from_slice(&(config.dimensions as u64).to_le_bytes());
        buffer.push(config.metric.to_u8());
        buffer.extend_from_slice(&(config.m as u64).to_le_bytes());
        buffer.extend_from_slice(&(config.ef_construction as u64).to_le_bytes());
        buffer.extend_from_slice(&(config.ef_search as u64).to_le_bytes());
        buffer.extend_from_slice(&(config.capacity as u64).to_le_bytes());
        // 8 + 1 + 8 + 8 + 8 + 8 = 41 bytes

        // We want read_exact to succeed for the first 41 bytes,
        // then fail when trying to read the 42nd byte (quantization).

        let mut reader = MockFailReader::new(buffer, 41);
        let result = HnswConfig::deserialize_from(&mut reader);

        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("Mock read error"));
    }
}
