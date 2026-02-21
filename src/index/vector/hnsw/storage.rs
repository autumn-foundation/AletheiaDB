//! HNSW index storage utilities.

use crate::core::error::{Error, Result, VectorError};
use crate::core::id::NodeId;
use crate::index::vector::{DistanceMetric, Quantization};
use crc32fast::Hasher;
use dashmap::DashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use super::config::{HnswConfig, IndexMetadata};

/// Magic bytes for mapping file identification
pub const MAPPING_MAGIC: &[u8; 4] = b"GMAP";
/// Current mapping file format version
pub const MAPPING_VERSION: u8 = 2;

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
pub const MAX_MAPPINGS_COUNT: usize = 100_000_000;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::MAX_VECTOR_DIMENSIONS;
    use crate::index::vector::hnsw::builder::HnswIndexBuilder;
    use crate::index::vector::hnsw::index::HnswIndex;
    use crate::index::vector::VectorIndex;

    #[test]
    fn test_load_mappings_bad_magic() -> Result<()> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_index.usearch");
        let mappings_path = path.with_extension("usearch.mappings");

        // Create valid index
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;
        index.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0])?;
        index.save(&path)?;

        // Corrupt magic bytes
        let mut data = std::fs::read(&mappings_path).unwrap();
        data[0] = b'X';
        data[1] = b'X';
        data[2] = b'X';
        data[3] = b'X';
        std::fs::write(&mappings_path, &data).unwrap();

        // Try to load
        let result = HnswIndex::load(&path, HnswConfig::new(4, DistanceMetric::Cosine));
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::IndexError(msg))) => {
                assert!(msg.contains("bad magic bytes"));
            }
            Ok(_) => panic!("Expected IndexError with bad magic bytes message, got: Ok(_)"),
            Err(e) => panic!(
                "Expected IndexError with bad magic bytes message, got: Err({:?})",
                e
            ),
        }
        Ok(())
    }

    #[test]
    fn test_load_mappings_bad_version() -> Result<()> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_index.usearch");
        let mappings_path = path.with_extension("usearch.mappings");

        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;
        index.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0])?;
        index.save(&path)?;

        // Corrupt version
        let mut data = std::fs::read(&mappings_path).unwrap();
        data[4] = 99; // Invalid version
        std::fs::write(&mappings_path, &data).unwrap();

        let result = HnswIndex::load(&path, HnswConfig::new(4, DistanceMetric::Cosine));
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::IndexError(msg))) => {
                assert!(msg.contains("Unsupported mapping file version"));
            }
            Ok(_) => panic!("Expected IndexError with version message, got: Ok(_)"),
            Err(e) => panic!(
                "Expected IndexError with version message, got: Err({:?})",
                e
            ),
        }
        Ok(())
    }

    #[test]
    fn test_load_mappings_bad_crc() -> Result<()> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_index.usearch");
        let mappings_path = path.with_extension("usearch.mappings");

        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;
        index.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0])?;
        index.save(&path)?;

        // Corrupt data (which invalidates CRC)
        let mut data = std::fs::read(&mappings_path).unwrap();
        // Modify the node ID part of the data
        // Header V2 size: 4(Magic) + 1(Ver) + 8(Dims) + 1(Quant) + 1(Metric) + 8(Count) = 23 bytes
        let header_size = 23;
        if data.len() > header_size {
            data[header_size] = data[header_size].wrapping_add(1);
        }
        std::fs::write(&mappings_path, &data).unwrap();

        let result = HnswIndex::load(&path, HnswConfig::new(4, DistanceMetric::Cosine));
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::IndexError(msg))) => {
                assert!(msg.contains("CRC mismatch"));
            }
            Ok(_) => panic!("Expected IndexError with CRC message, got: Ok(_)"),
            Err(e) => panic!("Expected IndexError with CRC message, got: Err({:?})", e),
        }
        Ok(())
    }

    #[test]
    fn test_load_mappings_truncated() -> Result<()> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_index.usearch");
        let mappings_path = path.with_extension("usearch.mappings");

        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;
        index.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0])?;
        index.save(&path)?;

        // Truncate file
        let data = std::fs::read(&mappings_path).unwrap();
        let truncated = &data[..10]; // Smaller than header
        std::fs::write(&mappings_path, truncated).unwrap();

        let result = HnswIndex::load(&path, HnswConfig::new(4, DistanceMetric::Cosine));
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::IndexError(msg))) => {
                assert!(msg.contains("too small") || msg.contains("corrupted"));
            }
            Ok(_) => panic!("Expected IndexError with size message, got: Ok(_)"),
            Err(e) => panic!("Expected IndexError with size message, got: Err({:?})", e),
        }
        Ok(())
    }

    #[test]
    fn test_load_mappings_size_mismatch() -> Result<()> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_index.usearch");
        let mappings_path = path.with_extension("usearch.mappings");

        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;
        index.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0])?;
        index.save(&path)?;

        // Modify count to be larger (mismatch with actual size), then fix CRC to pass CRC check
        let mut data = std::fs::read(&mappings_path).unwrap();
        // Count is at offset 15 (Magic 4 + Version 1 + Dims 8 + Quant 1 + Metric 1)
        // Original count is 1. Let's make it 2.
        let count_offset = 15;
        data[count_offset] = 2;

        // Recompute CRC so we pass the CRC check and hit the size check
        let crc_offset = data.len() - 4;
        let mut hasher = Hasher::new();
        hasher.update(&data[..crc_offset]);
        let new_crc = hasher.finalize();

        let crc_bytes = new_crc.to_le_bytes();
        data[crc_offset] = crc_bytes[0];
        data[crc_offset + 1] = crc_bytes[1];
        data[crc_offset + 2] = crc_bytes[2];
        data[crc_offset + 3] = crc_bytes[3];

        std::fs::write(&mappings_path, &data).unwrap();

        let result = HnswIndex::load(&path, HnswConfig::new(4, DistanceMetric::Cosine));
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::IndexError(msg))) => {
                assert!(msg.contains("size mismatch"));
            }
            Ok(_) => panic!("Expected IndexError with size mismatch message, got: Ok(_)"),
            Err(e) => panic!(
                "Expected IndexError with size mismatch message, got: Err({:?})",
                e
            ),
        }
        Ok(())
    }

    #[test]
    fn test_load_mappings_overflow_header() -> Result<()> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_index.usearch");
        let mappings_path = path.with_extension("usearch.mappings");

        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;
        index.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0])?;
        index.save(&path)?;

        // Modify count to be HUGE (u64::MAX) to trigger arithmetic overflow check
        let mut data = std::fs::read(&mappings_path).unwrap();
        let count_offset = 15; // V2 offset
        let huge_count = u64::MAX;

        // Write huge count
        let count_bytes = huge_count.to_le_bytes();
        data[count_offset..count_offset + 8].copy_from_slice(&count_bytes);

        // Update CRC (checksum calculation is still valid, only logic check fails)
        let crc_offset = data.len() - 4;
        let mut hasher = Hasher::new();
        hasher.update(&data[..crc_offset]);
        let new_crc = hasher.finalize();
        data[crc_offset..].copy_from_slice(&new_crc.to_le_bytes());

        std::fs::write(&mappings_path, &data).unwrap();

        let result = HnswIndex::load(&path, HnswConfig::new(4, DistanceMetric::Cosine));
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::IndexError(msg))) => {
                assert!(
                    msg.contains("overflow") || msg.contains("exceeds maximum allowed"),
                    "Expected overflow or max limit error, got: {}",
                    msg
                );
            }
            Ok(_) => panic!("Expected IndexError with overflow/limit message, got: Ok(_)"),
            Err(e) => panic!(
                "Expected IndexError with overflow message, got: Err({:?})",
                e
            ),
        }
        Ok(())
    }

    #[test]
    fn test_load_mappings_count_limit() -> Result<()> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_index.usearch");
        let mappings_path = path.with_extension("usearch.mappings");

        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;
        index.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0])?;
        index.save(&path)?;

        // Modify count to be MAX_MAPPINGS_COUNT + 1
        let mut data = std::fs::read(&mappings_path).unwrap();
        let count_offset = 15; // V2 offset
        let huge_count = (super::MAX_MAPPINGS_COUNT + 1) as u64;

        // Write huge count
        let count_bytes = huge_count.to_le_bytes();
        data[count_offset..count_offset + 8].copy_from_slice(&count_bytes);

        // Update CRC
        let crc_offset = data.len() - 4;
        let mut hasher = Hasher::new();
        hasher.update(&data[..crc_offset]);
        let new_crc = hasher.finalize();
        data[crc_offset..].copy_from_slice(&new_crc.to_le_bytes());

        std::fs::write(&mappings_path, &data).unwrap();

        let result = HnswIndex::load(&path, HnswConfig::new(4, DistanceMetric::Cosine));
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::IndexError(msg))) => {
                assert!(
                    msg.contains("exceeds maximum allowed"),
                    "Expected max limit error, got: {}",
                    msg
                );
            }
            Ok(_) => panic!("Expected IndexError with limit message, got: Ok(_)"),
            Err(e) => panic!("Expected IndexError with limit message, got: Err({:?})", e),
        }
        Ok(())
    }

    #[test]
    fn test_save_mappings_large_streaming() -> Result<()> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_streaming.usearch");

        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;

        // Add enough items to exceed typical buffer sizes (e.g. 8KB)
        // 2000 items * 16 bytes = 32KB
        let count = 2000;
        for i in 1..=count {
            index.add(NodeId::new(i).unwrap(), &[1.0, 0.0, 0.0, 0.0])?;
        }

        index.save(&path)?;

        // Verify we can load it back
        let loaded = HnswIndex::load(&path, HnswConfig::new(4, DistanceMetric::Cosine))?;
        assert_eq!(loaded.len(), count as usize);

        // Verify a few items
        let results = loaded.search(&[1.0, 0.0, 0.0, 0.0], 1)?;
        assert!(!results.is_empty());

        Ok(())
    }

    #[test]
    fn test_validate_metadata_dimensions_too_large() {
        let huge_dims = MAX_VECTOR_DIMENSIONS + 1;
        let metadata = Some(IndexMetadata {
            dimensions: huge_dims,
            quantization: Quantization::F32,
            metric: DistanceMetric::Cosine,
        });
        let config = HnswConfig::default();

        let result = HnswIndex::validate_metadata(metadata, &config);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("Stored index dimensions"));
        assert!(msg.contains("exceeds maximum allowed"));
    }

    // Mock writer that fails after writing N bytes
    struct MockFailWriter {
        fail_after: usize,
        written: usize,
    }

    impl MockFailWriter {
        fn new(fail_after: usize) -> Self {
            Self {
                fail_after,
                written: 0,
            }
        }
    }

    impl std::io::Write for MockFailWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if self.written + buf.len() > self.fail_after {
                return Err(std::io::Error::other("Mock write error"));
            }
            self.written += buf.len();
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            // Can simulate flush failure if needed, but write failure is sufficient for coverage
            Ok(())
        }
    }

    #[test]
    fn test_save_mappings_write_errors() {
        // Create dummy mappings
        let mappings = [
            (NodeId::new(1).unwrap(), 100),
            (NodeId::new(2).unwrap(), 200),
        ];

        let config = HnswConfig::default();

        // Case 1: Fail during header (MAGIC)
        // Magic is 4 bytes. Fail at byte 3.
        let mut writer = MockFailWriter::new(3);
        let result = super::write_mappings_to_writer(
            &mut writer,
            mappings.iter().copied(),
            mappings.len(),
            &config,
        );
        assert!(result.is_err());
        if let Err(Error::Vector(VectorError::IndexError(msg))) = result {
            assert!(msg.contains("Failed to write mappings"));
        } else {
            panic!("Expected IndexError");
        }

        // Case 2: Fail during data writing
        // V2 Header: Magic(4) + Version(1) + Dims(8) + Quant(1) + Metric(1) + Count(8) = 23 bytes
        // Data is 16 bytes per item.
        // Fail after header + 1st item (16 bytes) + 1 byte
        let mut writer = MockFailWriter::new(23 + 16 + 1);
        let result = super::write_mappings_to_writer(
            &mut writer,
            mappings.iter().copied(),
            mappings.len(),
            &config,
        );
        assert!(result.is_err());
        if let Err(Error::Vector(VectorError::IndexError(msg))) = result {
            assert!(msg.contains("Failed to write mappings"));
        }

        // Case 3: Fail during CRC writing
        // Total data size = 23 + 32 = 55 bytes.
        // CRC is 4 bytes.
        // Fail at 55 + 1 byte (during CRC write)
        let mut writer = MockFailWriter::new(55 + 1);
        let result = super::write_mappings_to_writer(
            &mut writer,
            mappings.iter().copied(),
            mappings.len(),
            &config,
        );
        assert!(result.is_err());
        if let Err(Error::Vector(VectorError::IndexError(msg))) = result {
            assert!(msg.contains("Failed to write CRC"));
        }
    }

    struct MockFlushFailWriter;
    impl std::io::Write for MockFlushFailWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::other("Mock flush error"))
        }
    }

    #[test]
    fn test_save_mappings_flush_error() {
        let mappings = [];
        let config = HnswConfig::default();
        let mut writer = MockFlushFailWriter;
        let result = super::write_mappings_to_writer(
            &mut writer,
            mappings.iter().copied(),
            mappings.len(),
            &config,
        );
        assert!(result.is_err());
        if let Err(Error::Vector(VectorError::IndexError(msg))) = result {
            assert!(msg.contains("Failed to flush mappings"));
        } else {
            panic!("Expected IndexError");
        }
    }
}
