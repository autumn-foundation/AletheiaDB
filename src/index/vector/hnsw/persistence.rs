use crate::core::error::{Error, Result, VectorError};
use crate::core::id::NodeId;
use crate::core::property::MAX_VECTOR_DIMENSIONS;
use crate::index::vector::{DistanceMetric, Quantization};
use super::config::HnswConfig;
use crc32fast::Hasher;
use dashmap::DashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

/// Magic bytes for mapping file identification
pub(crate) const MAPPING_MAGIC: &[u8; 4] = b"GMAP";
/// Current mapping file format version
pub(crate) const MAPPING_VERSION: u8 = 2;

/// Maximum number of entries allowed in a mappings file.
///
/// This limit prevents Memory Exhaustion DoS attacks where a malicious actor
/// provides a sparse mappings file with a header claiming billions of entries.
/// Loading such a file would cause `load_mappings_with_integrity` to attempt
/// allocating massive amounts of memory for the ID mapping `DashMap`.
///
/// Set to 100 Million (100_000_000), which is well above reasonable single-index limits
/// but low enough to prevent catastrophic OOM on typical servers.
/// 100M entries * (16 bytes data + ~32 bytes DashMap overhead) ≈ 4.8GB RAM.
pub(crate) const MAX_MAPPINGS_COUNT: usize = 100_000_000;

/// Metadata stored in the mappings file (Version 2+)
#[derive(Debug)]
pub(crate) struct IndexMetadata {
    pub dimensions: usize,
    pub quantization: Quantization,
    pub metric: DistanceMetric,
}

/// Helper method to stream mappings to a writer with CRC calculation.
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

    fn write_and_hash<W: Write>(
        writer: &mut W,
        hasher: &mut Hasher,
        data: &[u8],
    ) -> Result<()> {
        writer.write_all(data).map_err(|e| {
            Error::Vector(VectorError::IndexError(format!(
                "Failed to write mappings: {}",
                e
            )))
        })?;
        hasher.update(data);
        Ok(())
    }

    // Write header
    write_and_hash(writer, &mut hasher, MAPPING_MAGIC)?;
    write_and_hash(writer, &mut hasher, &[MAPPING_VERSION])?;

    // Version 2 fields: Dimensions, Quantization, Metric
    write_and_hash(
        writer,
        &mut hasher,
        &(config.dimensions as u64).to_le_bytes(),
    )?;
    write_and_hash(writer, &mut hasher, &[config.quantization.to_u8()])?;
    write_and_hash(writer, &mut hasher, &[config.metric.to_u8()])?;

    write_and_hash(writer, &mut hasher, &count_u64.to_le_bytes())?;

    // Write data directly
    for (node_id, key) in mappings_iter {
        write_and_hash(writer, &mut hasher, &node_id.as_u64().to_le_bytes())?;
        write_and_hash(writer, &mut hasher, &key.to_le_bytes())?;
    }

    // Calculate and write CRC32
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

/// Load and verify mappings from a companion file.
///
/// # Format
///
/// - **V1**: `[MAGIC:4][VERSION:1][COUNT:8][DATA:16*count][CRC32:4]`
/// - **V2**: `[MAGIC:4][VERSION:2][DIMS:8][QUANT:1][METRIC:1][COUNT:8][DATA:16*count][CRC32:4]`
///
/// # Integrity Checks
///
/// - **Magic Bytes**: Verifies file type (`GMAP`).
/// - **File Size**: Checked against expected size based on header count (prevents partial reads).
/// - **CRC32**: Verifies full file integrity.
/// - **Limits**: Enforces `MAX_MAPPINGS_COUNT` to prevent OOM DoS.
#[allow(clippy::type_complexity)]
pub(crate) fn load_mappings_with_integrity(
    mappings_path: &Path,
) -> Result<(
    DashMap<NodeId, u64>,
    DashMap<u64, NodeId>,
    u64,
    Option<IndexMetadata>,
)> {
    let id_mapping = DashMap::new();
    let reverse_mapping = DashMap::new();
    let mut max_key = 0u64;

    if !mappings_path.exists() {
        return Ok((id_mapping, reverse_mapping, max_key, None));
    }

    // Use streaming (File + BufReader) instead of reading entire file to memory (fs::read).
    // This prevents OOM DoS attacks with large or manipulated files.
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

    // Minimum size check (V1 min size)
    // Magic(4) + Version(1) + Count(8) + CRC(4) = 17 bytes
    if file_len < 17 {
        return Err(Error::Vector(VectorError::IndexError(
            "Mapping file too small or corrupted".to_string(),
        )));
    }

    let mut reader = std::io::BufReader::new(file);
    let mut hasher = Hasher::new();

    // 1. Read Start of Header (5 bytes: Magic + Version)
    let mut header_start = [0u8; 5];
    reader.read_exact(&mut header_start).map_err(|e| {
        Error::Vector(VectorError::IndexError(format!(
            "Failed to read mappings header start: {}",
            e
        )))
    })?;

    hasher.update(&header_start);

    // Verify magic bytes
    if &header_start[0..4] != MAPPING_MAGIC {
        return Err(Error::Vector(VectorError::IndexError(
            "Invalid mapping file: bad magic bytes".to_string(),
        )));
    }

    let version = header_start[4];

    // Read remaining header based on version
    let (count, metadata, header_overhead) = match version {
        1 => {
            // V1: Count(8)
            let mut buf = [0u8; 8];
            reader.read_exact(&mut buf).map_err(|e| {
                Error::Vector(VectorError::IndexError(format!(
                    "Failed to read V1 header fields: {}",
                    e
                )))
            })?;
            hasher.update(&buf);
            let count = u64::from_le_bytes(buf) as usize;
            // Overhead: Magic(4) + Version(1) + Count(8) + CRC(4) = 17
            (count, None, 17)
        }
        2 => {
            // V2: Dims(8) + Quant(1) + Metric(1) + Count(8)
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
            // Overhead: Magic(4) + Version(1) + Dims(8) + Quant(1) + Metric(1) + Count(8) + CRC(4) = 27
            (count, Some(meta), 27)
        }
        v => {
            return Err(Error::Vector(VectorError::IndexError(format!(
                "Unsupported mapping file version: {} (expected 1 or {})",
                v, MAPPING_VERSION
            ))));
        }
    };

    // Security Check: Enforce maximum mappings count to prevent OOM DoS
    if count > MAX_MAPPINGS_COUNT {
        return Err(Error::Vector(VectorError::IndexError(format!(
            "Mappings count {} exceeds maximum allowed {}",
            count, MAX_MAPPINGS_COUNT
        ))));
    }

    // Verify data size with checked arithmetic
    // Cast to u64 for file size comparison
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

    // Critical Security Check: Verify file size matches expected size BEFORE reading data.
    // This prevents reading until EOF if the file is truncated or huge.
    if file_len != expected_size {
        return Err(Error::Vector(VectorError::IndexError(format!(
            "Mapping file size mismatch: expected {} bytes, got {}",
            expected_size, file_len
        ))));
    }

    // 2. Read Data
    // We read in chunks to avoid allocating a huge buffer, but large enough for efficiency.
    // 16KB buffer holds 1024 entries.
    const CHUNK_SIZE: usize = 1024 * 16;
    let mut buffer = vec![0u8; CHUNK_SIZE];
    let mut remaining_entries = count;

    while remaining_entries > 0 {
        // Calculate entries for this chunk
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

    // 3. Read and Verify CRC
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

    Ok((id_mapping, reverse_mapping, max_key, metadata))
}

/// Verify that the binary index file matches the expected dimensions and quantization.
///
/// This reads the first 8 bytes of the file to check the vector size field.
/// usearch stores `count` (bytes 0-3) and `vector_byte_size` (bytes 4-7).
/// We verify that `vector_byte_size == dimensions * scalar_size`.
pub(crate) fn verify_index_header(
    path: &Path,
    dimensions: usize,
    quantization: Quantization,
) -> Result<()> {
    let mut file = File::open(path).map_err(|e| {
        Error::Vector(VectorError::IndexError(format!(
            "Failed to open index file for verification: {}",
            e
        )))
    })?;

    let mut header = [0u8; 8];
    file.read_exact(&mut header).map_err(|e| {
        Error::Vector(VectorError::IndexError(format!(
            "Failed to read index header: {}",
            e
        )))
    })?;

    // Extract vector_byte_size from bytes 4-7 (little-endian u32)
    let vector_byte_size = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;

    let scalar_size = match quantization {
        Quantization::F32 => 4,
        Quantization::F16 => 2,
        Quantization::I8 => 1,
    };

    let expected_size = dimensions * scalar_size;

    if vector_byte_size != expected_size {
        return Err(Error::Vector(VectorError::IndexError(format!(
            "Index file header mismatch: expected {} bytes per vector ({} dims * {} bytes), found {}",
            expected_size, dimensions, scalar_size, vector_byte_size
        ))));
    }

    Ok(())
}

/// Validate loaded index metadata against configuration.
pub(crate) fn validate_metadata(metadata: Option<IndexMetadata>, config: &HnswConfig) -> Result<()> {
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
        // Legacy index (Version 1)
        // Prevent usage of custom metric with legacy index to avoid buffer over-read vulnerability
        // (since we cannot verify dimensions/quantization)
        if config.custom_metric.is_some() {
            return Err(Error::Vector(VectorError::IndexError(
                "Cannot use custom metric with legacy index (missing metadata validation)"
                    .to_string(),
            )));
        }
    }
    Ok(())
}
