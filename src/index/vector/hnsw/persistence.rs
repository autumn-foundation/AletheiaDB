use crate::core::error::{Error, Result, VectorError};
use crate::core::id::NodeId;
use crate::core::property::MAX_VECTOR_DIMENSIONS;
use crate::index::vector::{DistanceMetric, Quantization, StorageMode};
use crc32fast::Hasher;
use dashmap::DashMap;
use parking_lot::{Mutex, RwLock};
use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use usearch::{Index, IndexOptions};

use super::config::{HnswConfig, create_metric_wrapper, to_usearch_metric, to_usearch_scalar};
use super::{HnswIndex, IndexStats, MAX_K, NUM_ENTRY_LOCKS};

/// Magic bytes for mapping file identification
pub(crate) const MAPPING_MAGIC: &[u8; 4] = b"GMAP";
/// Current mapping file format version
pub(crate) const MAPPING_VERSION: u8 = 2;

/// Maximum number of entries allowed in a mappings file.
pub(crate) const MAX_MAPPINGS_COUNT: usize = 100_000_000;

/// Metadata stored in the mappings file (Version 2+)
pub(crate) struct IndexMetadata {
    pub dimensions: usize,
    pub quantization: Quantization,
    pub metric: DistanceMetric,
}

pub(crate) struct LoadedMappings {
    pub id_mapping: DashMap<NodeId, u64>,
    pub reverse_mapping: DashMap<u64, NodeId>,
    pub max_key: u64,
    pub metadata: Option<IndexMetadata>,
}

pub(crate) fn save_index(index: &HnswIndex, path: &Path) -> Result<()> {
    // Acquire save_lock (exclusive) to prevent concurrent adds/removes.
    let _save_guard = index.save_lock.write();

    // Acquire inner write lock to serialize `save` with concurrent searches.
    let inner_index = index.inner.write();

    // Collect mappings while holding the locks.
    let mappings: Vec<(NodeId, u64)> = index
        .id_mapping
        .iter()
        .map(|e| (*e.key(), *e.value()))
        .collect();
    let count = mappings.len();

    inner_index
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
    // Explicit drop to release lock before I/O
    drop(inner_index);
    drop(_save_guard);

    // Save mappings to companion file with integrity checks
    let mappings_path = path.with_extension("usearch.mappings");

    // Calculate total size: Magic(4) + Version(1) + Dims(8) + Quant(1) + Metric(1) + Count(8) + Data(count * 16) + CRC(4)
    let count_size = count
        .checked_mul(16)
        .ok_or_else(|| Error::Vector(VectorError::IndexError("Index too large".to_string())))?;
    let _total_size = count_size
        .checked_add(4 + 1 + 8 + 1 + 1 + 8 + 4)
        .ok_or_else(|| Error::Vector(VectorError::IndexError("Index too large".to_string())))?;

    let file = File::create(&mappings_path).map_err(|e| {
        Error::Vector(VectorError::IndexError(format!(
            "Failed to create mappings file: {}",
            e
        )))
    })?;
    let mut writer = BufWriter::new(file);

    write_mappings_to_writer(&mut writer, mappings.into_iter(), count, &index.config)
}

pub(crate) fn write_mappings_to_writer<W, I>(
    writer: &mut W,
    mappings_iter: I,
    count: usize,
    config: &HnswConfig,
) -> Result<()>
where
    W: Write,
    I: Iterator<Item = (NodeId, u64)>,
{
    let mut hasher = Hasher::new();
    let count_u64 = count as u64;

    fn write_and_hash<W: Write>(writer: &mut W, hasher: &mut Hasher, data: &[u8]) -> Result<()> {
        writer.write_all(data).map_err(|e| {
            Error::Vector(VectorError::IndexError(format!(
                "Failed to write mappings: {}",
                e
            )))
        })?;
        hasher.update(data);
        Ok(())
    }

    write_and_hash(writer, &mut hasher, MAPPING_MAGIC)?;
    write_and_hash(writer, &mut hasher, &[MAPPING_VERSION])?;

    write_and_hash(
        writer,
        &mut hasher,
        &(config.dimensions as u64).to_le_bytes(),
    )?;
    write_and_hash(writer, &mut hasher, &[config.quantization.to_u8()])?;
    write_and_hash(writer, &mut hasher, &[config.metric.to_u8()])?;

    write_and_hash(writer, &mut hasher, &count_u64.to_le_bytes())?;

    for (node_id, key) in mappings_iter {
        write_and_hash(writer, &mut hasher, &node_id.as_u64().to_le_bytes())?;
        write_and_hash(writer, &mut hasher, &key.to_le_bytes())?;
    }

    let crc = hasher.finalize();
    writer.write_all(&crc.to_le_bytes()).map_err(|e| {
        Error::Vector(VectorError::IndexError(format!(
            "Failed to write CRC: {}",
            e
        )))
    })?;

    writer.flush().map_err(|e| {
        Error::Vector(VectorError::IndexError(format!(
            "Failed to flush mappings: {}",
            e
        )))
    })?;

    Ok(())
}

pub(crate) fn load_index(path: &Path, config: HnswConfig) -> Result<HnswIndex> {
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
    let loaded = load_mappings_with_integrity(&mappings_path)?;

    validate_metadata(loaded.metadata, &config)?;

    Ok(HnswIndex {
        inner: Arc::new(RwLock::new(index)),
        config,
        id_mapping: Arc::new(loaded.id_mapping),
        reverse_mapping: Arc::new(loaded.reverse_mapping),
        next_key: AtomicU64::new(loaded.max_key + 1),
        stats: Arc::new(IndexStats::default()),
        max_k: MAX_K,
        is_mmap: false,
        save_lock: Arc::new(RwLock::new(())),
        entry_locks: (0..NUM_ENTRY_LOCKS).map(|_| Mutex::new(())).collect(),
    })
}

pub(crate) fn open_mmap_index(path: &Path) -> Result<HnswIndex> {
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
    let loaded = load_mappings_with_integrity(&mappings_path)?;

    let (quantization, metric) = if let Some(meta) = loaded.metadata {
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
            storage: StorageMode::MemoryMapped {
                path: path.to_path_buf(),
            },
            ..Default::default()
        },
        id_mapping: Arc::new(loaded.id_mapping),
        reverse_mapping: Arc::new(loaded.reverse_mapping),
        next_key: AtomicU64::new(loaded.max_key + 1),
        stats: Arc::new(IndexStats::default()),
        max_k: MAX_K,
        is_mmap: true,
        save_lock: Arc::new(RwLock::new(())),
        entry_locks: (0..NUM_ENTRY_LOCKS).map(|_| Mutex::new(())).collect(),
    })
}

pub(crate) fn load_mappings_with_integrity(mappings_path: &Path) -> Result<LoadedMappings> {
    let id_mapping = DashMap::new();
    let reverse_mapping = DashMap::new();
    let mut max_key = 0u64;

    if !mappings_path.exists() {
        return Ok(LoadedMappings {
            id_mapping,
            reverse_mapping,
            max_key,
            metadata: None,
        });
    }

    let file = File::open(mappings_path).map_err(|e| {
        Error::Vector(VectorError::IndexError(format!(
            "Failed to open mappings file: {}",
            e
        )))
    })?;

    let file_len = file
        .metadata()
        .map_err(|e| {
            Error::Vector(VectorError::IndexError(format!(
                "Failed to get mappings file metadata: {}",
                e
            )))
        })?
        .len();

    if file_len < 17 {
        return Err(Error::Vector(VectorError::IndexError(
            "Mapping file too small or corrupted".to_string(),
        )));
    }

    let mut reader = std::io::BufReader::new(file);
    let mut hasher = Hasher::new();

    let mut header_start = [0u8; 5];
    reader.read_exact(&mut header_start).map_err(|e| {
        Error::Vector(VectorError::IndexError(format!(
            "Failed to read mappings header start: {}",
            e
        )))
    })?;

    hasher.update(&header_start);

    if &header_start[0..4] != MAPPING_MAGIC {
        return Err(Error::Vector(VectorError::IndexError(
            "Invalid mapping file: bad magic bytes".to_string(),
        )));
    }

    let version = header_start[4];

    let (count, metadata, header_overhead) = match version {
        1 => {
            let mut buf = [0u8; 8];
            reader.read_exact(&mut buf).map_err(|e| {
                Error::Vector(VectorError::IndexError(format!(
                    "Failed to read V1 header fields: {}",
                    e
                )))
            })?;
            hasher.update(&buf);
            let count = u64::from_le_bytes(buf) as usize;
            (count, None, 17)
        }
        2 => {
            let mut buf = [0u8; 18];
            reader.read_exact(&mut buf).map_err(|e| {
                Error::Vector(VectorError::IndexError(format!(
                    "Failed to read V2 header fields: {}",
                    e
                )))
            })?;
            hasher.update(&buf);

            let dims = u64::from_le_bytes(buf[0..8].try_into().unwrap()) as usize;
            let quant = Quantization::from_u8(buf[8])?;
            let metric = DistanceMetric::from_u8(buf[9])?;
            let count = u64::from_le_bytes(buf[10..18].try_into().unwrap()) as usize;

            let meta = IndexMetadata {
                dimensions: dims,
                quantization: quant,
                metric,
            };
            (count, Some(meta), 27)
        }
        v => {
            return Err(Error::Vector(VectorError::IndexError(format!(
                "Unsupported mapping file version: {} (expected 1 or {})",
                v, MAPPING_VERSION
            ))));
        }
    };

    if count > MAX_MAPPINGS_COUNT {
        return Err(Error::Vector(VectorError::IndexError(format!(
            "Mappings count {} exceeds maximum allowed {}",
            count, MAX_MAPPINGS_COUNT
        ))));
    }

    let data_size = (count as u64).checked_mul(16).ok_or_else(|| {
        Error::Vector(VectorError::IndexError(
            "Mapping count too large (overflow)".to_string(),
        ))
    })?;
    let expected_size = data_size.checked_add(header_overhead).ok_or_else(|| {
        Error::Vector(VectorError::IndexError(
            "Mapping file size too large (overflow)".to_string(),
        ))
    })?;

    if file_len != expected_size {
        return Err(Error::Vector(VectorError::IndexError(format!(
            "Mapping file size mismatch: expected {} bytes, got {}",
            expected_size, file_len
        ))));
    }

    const CHUNK_SIZE: usize = 1024 * 16;
    let mut buffer = vec![0u8; CHUNK_SIZE];
    let mut remaining_entries = count;

    while remaining_entries > 0 {
        let entries_in_chunk = std::cmp::min(remaining_entries, 1024);
        let bytes_to_read = entries_in_chunk * 16;
        let slice = &mut buffer[0..bytes_to_read];

        reader.read_exact(slice).map_err(|e| {
            Error::Vector(VectorError::IndexError(format!(
                "Failed to read mappings data: {}",
                e
            )))
        })?;

        hasher.update(slice);

        for chunk in slice.chunks_exact(16) {
            let node_id_raw = u64::from_le_bytes(chunk[0..8].try_into().unwrap());
            let key = u64::from_le_bytes(chunk[8..16].try_into().unwrap());

            if let Ok(node_id) = NodeId::new(node_id_raw) {
                id_mapping.insert(node_id, key);
                reverse_mapping.insert(key, node_id);
                max_key = max_key.max(key);
            }
        }

        remaining_entries -= entries_in_chunk;
    }

    let mut crc_buf = [0u8; 4];
    reader.read_exact(&mut crc_buf).map_err(|e| {
        Error::Vector(VectorError::IndexError(format!(
            "Failed to read mappings CRC: {}",
            e
        )))
    })?;

    let stored_crc = u32::from_le_bytes(crc_buf);
    let computed_crc = hasher.finalize();

    if stored_crc != computed_crc {
        return Err(Error::Vector(VectorError::IndexError(format!(
            "Mapping file corrupted: CRC mismatch (stored: {}, computed: {})",
            stored_crc, computed_crc
        ))));
    }

    Ok(LoadedMappings {
        id_mapping,
        reverse_mapping,
        max_key,
        metadata,
    })
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
    } else if config.custom_metric.is_some() {
        return Err(Error::Vector(VectorError::IndexError(
            "Cannot use custom metric with legacy index (missing metadata validation)".to_string(),
        )));
    }
    Ok(())
}
