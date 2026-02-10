use aletheiadb::core::property::{MAX_VECTOR_DIMENSIONS, deserialize_sparse_vector};
use aletheiadb::index::vector::{DistanceMetric, HnswConfig, HnswIndexBuilder, Quantization};
use aletheiadb::utils::error::{Error, StorageError, VectorError};

const TAG_SPARSE_VECTOR: u8 = 8;

#[test]
fn test_deserialize_sparse_vector_dimension_overflow() {
    // 🦠 Threat: Malicious input provides dimension = u32::MAX.
    // deserialization reads dimension, validates nnz, then calls SparseVec::new.
    // SparseVec::new MUST check dimension against MAX_VECTOR_DIMENSIONS.

    let dimension = u32::MAX;
    let nnz = 0u32;

    let mut buffer = Vec::new();
    buffer.push(TAG_SPARSE_VECTOR);
    buffer.extend_from_slice(&dimension.to_le_bytes());
    buffer.extend_from_slice(&nnz.to_le_bytes());

    let result = deserialize_sparse_vector(&buffer);

    assert!(result.is_err());
    match result {
        Err(Error::Vector(VectorError::DimensionTooLarge {
            dimension: d,
            max_allowed,
        })) => {
            assert_eq!(d, dimension as usize);
            assert_eq!(max_allowed, MAX_VECTOR_DIMENSIONS);
        }
        _ => panic!("Expected DimensionTooLarge error, got {:?}", result),
    }
}

#[test]
fn test_deserialize_sparse_vector_nnz_overflow() {
    // 🦠 Threat: nnz > dimension.
    let dimension = 10u32;
    let nnz = 11u32;

    let mut buffer = Vec::new();
    buffer.push(TAG_SPARSE_VECTOR);
    buffer.extend_from_slice(&dimension.to_le_bytes());
    buffer.extend_from_slice(&nnz.to_le_bytes());

    let result = deserialize_sparse_vector(&buffer);

    assert!(result.is_err());
    match result {
        Err(Error::Storage(StorageError::CorruptedData(msg))) => {
            assert!(msg.contains("exceeds dimension"));
        }
        _ => panic!("Expected CorruptedData error, got {:?}", result),
    }
}

#[test]
fn test_deserialize_sparse_vector_duplicate_indices() {
    // 🦠 Threat: Duplicate indices.
    let dimension = 10u32;
    let nnz = 2u32;

    let mut buffer = Vec::new();
    buffer.push(TAG_SPARSE_VECTOR);
    buffer.extend_from_slice(&dimension.to_le_bytes());
    buffer.extend_from_slice(&nnz.to_le_bytes());

    // Indices: [1, 1]
    buffer.extend_from_slice(&1u32.to_le_bytes());
    buffer.extend_from_slice(&1u32.to_le_bytes());

    // Values: [1.0, 2.0]
    buffer.extend_from_slice(&1.0f32.to_le_bytes());
    buffer.extend_from_slice(&2.0f32.to_le_bytes());

    let result = deserialize_sparse_vector(&buffer);

    assert!(result.is_err());
    match result {
        Err(Error::Vector(VectorError::InvalidSparseVector { reason })) => {
            assert!(reason.contains("Duplicate index"));
        }
        _ => panic!("Expected InvalidSparseVector error, got {:?}", result),
    }
}

#[test]
fn test_deserialize_sparse_vector_unsorted_indices() {
    // 🦠 Case: Unsorted indices should be sorted automatically.
    let dimension = 10u32;
    let nnz = 2u32;

    let mut buffer = Vec::new();
    buffer.push(TAG_SPARSE_VECTOR);
    buffer.extend_from_slice(&dimension.to_le_bytes());
    buffer.extend_from_slice(&nnz.to_le_bytes());

    // Indices: [5, 1]
    buffer.extend_from_slice(&5u32.to_le_bytes());
    buffer.extend_from_slice(&1u32.to_le_bytes());

    // Values: [5.0, 1.0] (corresponding to indices)
    buffer.extend_from_slice(&5.0f32.to_le_bytes());
    buffer.extend_from_slice(&1.0f32.to_le_bytes());

    let (sv, _) = deserialize_sparse_vector(&buffer).expect("Should succeed");

    assert_eq!(sv.indices(), &[1, 5]);
    assert_eq!(sv.values(), &[1.0, 5.0]);
}

#[test]
fn test_hnsw_builder_rejects_unsafe_quantization() {
    // 🦠 Threat: Using custom metric with non-F32 quantization causes potential buffer over-read
    // because custom metric wrapper assumes F32.
    // Defense: Builder must reject this config.

    let config = HnswConfig::new(128, DistanceMetric::Cosine)
        .with_quantization(Quantization::F16) // Unsafe combination with custom metric
        .with_custom_metric("unsafe_test", |a, b| {
            a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
        });

    let builder = HnswIndexBuilder::from_config(&config);
    let result = builder.build();

    assert!(result.is_err());
    match result {
        Err(Error::Vector(VectorError::InvalidVector { reason })) => {
            assert!(reason.contains("Custom metrics are only supported with F32 quantization"));
        }
        _ => panic!("Expected InvalidVector error, got {:?}", result),
    }
}
