use aletheiadb::index::vector::{DistanceMetric, HnswConfig, Quantization};
use aletheiadb::utils::Result;
use std::io::Cursor;

#[test]
fn test_quantization_conversion_coverage() {
    // Test all to_u8 variants
    assert_eq!(Quantization::F32.to_u8(), 0);
    assert_eq!(Quantization::F16.to_u8(), 1);
    assert_eq!(Quantization::I8.to_u8(), 2);

    // Test all from_u8 valid variants
    assert_eq!(Quantization::from_u8(0).unwrap(), Quantization::F32);
    assert_eq!(Quantization::from_u8(1).unwrap(), Quantization::F16);
    assert_eq!(Quantization::from_u8(2).unwrap(), Quantization::I8);

    // Test from_u8 invalid variants (error path)
    assert!(Quantization::from_u8(3).is_err());
    assert!(Quantization::from_u8(255).is_err());
}

#[test]
fn test_hnsw_config_deserialize_backward_compatibility() -> Result<()> {
    // Manually construct a serialized config WITHOUT the quantization byte (old format)
    let dimensions = 128u64;
    let metric = DistanceMetric::Cosine;
    let m = 16u64;
    let ef_construction = 100u64;
    let ef_search = 50u64;
    let capacity = 1000u64;

    let mut buffer = Vec::new();
    // dimensions (8)
    buffer.extend_from_slice(&dimensions.to_le_bytes());
    // metric (1)
    buffer.push(metric.to_u8());
    // m (8)
    buffer.extend_from_slice(&m.to_le_bytes());
    // ef_construction (8)
    buffer.extend_from_slice(&ef_construction.to_le_bytes());
    // ef_search (8)
    buffer.extend_from_slice(&ef_search.to_le_bytes());
    // capacity (8)
    buffer.extend_from_slice(&capacity.to_le_bytes());

    // STOP HERE: Do not write quantization byte. This simulates an old file ending at 41 bytes.

    let mut cursor = Cursor::new(buffer);

    // Deserialize should succeed and default to F32
    let config = HnswConfig::deserialize_from(&mut cursor)?;

    assert_eq!(config.dimensions, 128);
    assert_eq!(config.quantization, Quantization::F32); // Default behavior triggered by UnexpectedEof

    Ok(())
}

#[test]
fn test_hnsw_config_deserialize_io_error_propagation() {
    // Create a reader that returns a generic IO error (not UnexpectedEof)
    struct ErrorReader;
    impl std::io::Read for ErrorReader {
        fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "Generic failure",
            ))
        }
    }

    // We need to pass the first few reads to get to the quantization part?
    // Actually, deserialize_from reads many fields first.
    // If we want to test the specific error path at the END (quantization read),
    // we need a reader that succeeds for 41 bytes then fails with Other.

    struct FailAtEndReader {
        data: Vec<u8>,
        pos: usize,
    }

    impl FailAtEndReader {
        fn new(data: Vec<u8>) -> Self {
            Self { data, pos: 0 }
        }
    }

    impl std::io::Read for FailAtEndReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.pos >= self.data.len() {
                // If we've read all data (41 bytes), return an error instead of 0 (EOF)
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "Simulated read error",
                ));
            }
            let len = std::cmp::min(buf.len(), self.data.len() - self.pos);
            buf[..len].copy_from_slice(&self.data[self.pos..self.pos + len]);
            self.pos += len;
            Ok(len)
        }
    }

    // Construct valid 41-byte data
    let mut buffer = Vec::new();
    buffer.extend_from_slice(&128u64.to_le_bytes());
    buffer.push(0); // Cosine
    buffer.extend_from_slice(&16u64.to_le_bytes());
    buffer.extend_from_slice(&100u64.to_le_bytes());
    buffer.extend_from_slice(&50u64.to_le_bytes());
    buffer.extend_from_slice(&1000u64.to_le_bytes());

    let mut reader = FailAtEndReader::new(buffer);
    let result = HnswConfig::deserialize_from(&mut reader);

    assert!(result.is_err());
    let err = result.unwrap_err();
    // Should be our simulated error, not UnexpectedEof
    assert!(err.to_string().contains("Simulated read error"));
}
