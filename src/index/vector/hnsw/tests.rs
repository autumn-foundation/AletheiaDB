use super::*;
use crate::core::id::NodeId;
use crate::index::vector::{DistanceMetric, Quantization, StorageMode};
use std::sync::atomic::Ordering;

#[test]
fn test_metric_wrapper_safe_on_unaligned() {
    let distance_fn = Arc::new(|_: &[f32], _: &[f32]| 0.0);
    let wrapper = create_metric_wrapper(4, distance_fn);

    let buffer = [0u8; 32];
    let unaligned_ptr = unsafe { buffer.as_ptr().add(1) } as *const f32;
    let aligned_vec = [0.0f32; 4];
    let aligned_ptr = aligned_vec.as_ptr();

    let result = wrapper(unaligned_ptr, aligned_ptr);
    assert_eq!(result, f32::MAX);
}

#[test]
fn test_is_retryable_error_matching() {
    assert!(is_retryable_usearch_error(
        "Error: No available threads to lock for search"
    ));
    assert!(!is_retryable_usearch_error("Other error"));
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
fn test_builder_validation_limits() {
    let res = HnswIndexBuilder::new(10, DistanceMetric::Cosine)
        .m(100)
        .build();
    assert!(res.is_err());

    let res = HnswIndexBuilder::new(10, DistanceMetric::Cosine)
        .m(0)
        .build();
    assert!(res.is_err());

    let res = HnswIndexBuilder::new(0, DistanceMetric::Cosine).build();
    assert!(res.is_err());
}

#[test]
fn test_custom_metric_safety_check() {
    let result = HnswIndexBuilder::new(128, DistanceMetric::Cosine)
        .quantization(Quantization::I8)
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
fn test_hnsw_basic() -> Result<()> {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;

    let node1 = NodeId::new(1).unwrap();
    let node2 = NodeId::new(2).unwrap();

    index.add(node1, &[1.0, 0.0, 0.0, 0.0])?;
    index.add(node2, &[0.0, 1.0, 0.0, 0.0])?;

    assert_eq!(index.len(), 2);

    let results = index.search(&[1.0, 0.0, 0.0, 0.0], 2)?;
    assert_eq!(results[0].0, node1);

    Ok(())
}

#[test]
fn test_search_results_are_sorted() -> Result<()> {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .m(16)
        .ef_construction(100)
        .build()?;

    use rand::Rng;
    let mut rng = rand::thread_rng();
    for i in 1..=100 {
        let vec: Vec<f32> = (0..4).map(|_| rng.r#gen()).collect();
        index.add(NodeId::new(i).unwrap(), &vec)?;
    }

    let query: Vec<f32> = (0..4).map(|_| rng.r#gen()).collect();
    let results = index.search(&query, 20)?;

    for i in 0..results.len().saturating_sub(1) {
        assert!(
            results[i].1 >= results[i + 1].1,
            "Results unsorted at index {}: {} < {}",
            i,
            results[i].1,
            results[i + 1].1
        );
    }
    Ok(())
}

#[test]
fn test_dot_product_similarity_metric() -> Result<()> {
    let index = HnswIndexBuilder::new(2, DistanceMetric::DotProduct).build()?;
    let node = NodeId::new(1).unwrap();
    index.add(node, &[1.0, 0.0])?;

    let results = index.search(&[1.0, 0.0], 1)?;
    assert_eq!(results.len(), 1);
    let similarity = results[0].1;

    assert!(
        (similarity - 1.0).abs() < 0.001,
        "Expected 1.0, got {}",
        similarity
    );

    Ok(())
}

#[test]
fn test_hnsw_remove() -> Result<()> {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;

    let node1 = NodeId::new(1).unwrap();
    let node2 = NodeId::new(2).unwrap();

    index.add(node1, &[1.0, 0.0, 0.0, 0.0])?;
    index.add(node2, &[0.0, 1.0, 0.0, 0.0])?;

    assert_eq!(index.len(), 2);

    index.remove(node1)?;

    assert_eq!(index.len(), 1);

    let results = index.search(&[1.0, 0.0, 0.0, 0.0], 2)?;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, node2);

    Ok(())
}

#[test]
fn test_hnsw_search_with_filter() -> Result<()> {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;

    let node1 = NodeId::new(1).unwrap();
    let node2 = NodeId::new(2).unwrap();
    let node3 = NodeId::new(3).unwrap();

    index.add(node1, &[1.0, 0.0, 0.0, 0.0])?;
    index.add(node2, &[0.9, 0.1, 0.0, 0.0])?;
    index.add(node3, &[0.8, 0.2, 0.0, 0.0])?;

    let results =
        index.search_with_filter(&[1.0, 0.0, 0.0, 0.0], 3, |id| id.as_u64() % 2 == 0)?;

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, node2);

    Ok(())
}

#[test]
fn test_update_existing_node() -> Result<()> {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;

    let node1 = NodeId::new(1).unwrap();

    index.add(node1, &[1.0, 0.0, 0.0, 0.0])?;
    assert_eq!(index.len(), 1);

    index.add(node1, &[0.0, 1.0, 0.0, 0.0])?;
    assert_eq!(index.len(), 1);

    let results = index.search(&[0.0, 1.0, 0.0, 0.0], 1)?;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, node1);
    assert!(results[0].1 > 0.99);

    Ok(())
}

#[test]
fn test_capacity_expansion_on_add() -> Result<()> {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .initial_capacity(2)
        .build()?;

    let node1 = NodeId::new(1).unwrap();
    let node2 = NodeId::new(2).unwrap();
    index.add(node1, &[1.0, 0.0, 0.0, 0.0])?;
    index.add(node2, &[0.0, 1.0, 0.0, 0.0])?;
    assert_eq!(index.len(), 2);

    let node3 = NodeId::new(3).unwrap();
    index.add(node3, &[0.0, 0.0, 1.0, 0.0])?;
    assert_eq!(index.len(), 3);

    let node4 = NodeId::new(4).unwrap();
    let node5 = NodeId::new(5).unwrap();
    index.add(node4, &[0.0, 0.0, 0.0, 1.0])?;
    index.add(node5, &[0.5, 0.5, 0.0, 0.0])?;
    assert_eq!(index.len(), 5);

    let results = index.search(&[1.0, 0.0, 0.0, 0.0], 5)?;
    assert_eq!(results.len(), 5);

    Ok(())
}

#[test]
fn test_concurrent_update_same_node() -> Result<()> {
    use std::sync::Arc;
    use std::thread;

    let index = Arc::new(HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?);
    let node1 = NodeId::new(1).unwrap();

    index.add(node1, &[1.0, 0.0, 0.0, 0.0])?;

    let num_threads = 10;
    let updates_per_thread = 10;

    let mut handles = vec![];

    for thread_id in 0..num_threads {
        let index_clone = Arc::clone(&index);
        let handle = thread::spawn(move || {
            for i in 0..updates_per_thread {
                let val = (thread_id * updates_per_thread + i) as f32 / 100.0;
                let vector = vec![val, 1.0 - val, 0.0, 0.0];
                index_clone.add(node1, &vector).unwrap();
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }

    assert_eq!(index.len(), 1);

    let results = index.search(&[0.5, 0.5, 0.0, 0.0], 1)?;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, node1);

    Ok(())
}

#[test]
fn test_max_key_overflow_protection() -> Result<()> {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;

    const MAX_VALID_KEY: u64 = u64::MAX - 1000;
    index
        .next_key
        .store(MAX_VALID_KEY, std::sync::atomic::Ordering::SeqCst);

    let node1 = NodeId::new(1).unwrap();
    assert!(index.add(node1, &[1.0, 0.0, 0.0, 0.0]).is_ok());

    let node2 = NodeId::new(2).unwrap();
    let result = index.add(node2, &[0.0, 1.0, 0.0, 0.0]);
    assert!(result.is_err());

    if let Err(Error::Vector(VectorError::IndexError(msg))) = result {
        assert!(msg.contains("overflow") || msg.contains("exceeded"));
    } else {
        panic!("Expected IndexError");
    }

    Ok(())
}

#[test]
fn test_save_mappings_write_errors() {
    let mappings = [
        (NodeId::new(1).unwrap(), 100),
        (NodeId::new(2).unwrap(), 200),
    ];

    let config = HnswConfig::default();

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
            Ok(())
        }
    }

    // Case 1: Fail during header
    let mut writer = MockFailWriter::new(3);
    let result = super::persistence::write_mappings_to_writer(
        &mut writer,
        mappings.iter().copied(),
        mappings.len(),
        &config,
    );
    assert!(result.is_err());
}

#[test]
fn test_custom_metric_execution_coverage() {
    let metric_fn = |a: &[f32], b: &[f32]| -> f32 {
        a.iter().zip(b.iter()).map(|(x, y)| (x - y).abs()).sum()
    };

    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .quantization(Quantization::F32)
        .with_custom_metric("manhattan", metric_fn)
        .build()
        .unwrap();

    for i in 0..10 {
        let id = NodeId::new(i + 1).unwrap();
        let vec = if i % 2 == 0 {
            [1.0, 0.0, 0.0, 0.0]
        } else {
            [0.0, 1.0, 0.0, 0.0]
        };
        index.add(id, &vec).unwrap();
    }

    let results = index.search(&[0.9, 0.1, 0.0, 0.0], 5).unwrap();
    assert_eq!(results.len(), 5);
}

#[test]
fn test_add_reentrancy_check() {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build().unwrap();
    let node_id = NodeId::new(1).unwrap();
    let vec = vec![1.0, 0.0, 0.0, 0.0];

    let _guard = FilterCallbackGuard::new();

    let result = index.add(node_id, &vec);
    assert!(result.is_err());
    match result {
        Err(Error::Vector(VectorError::IndexError(msg))) => {
            assert!(
                msg.contains("Cannot modify index from within a search_with_filter callback")
            );
        }
        _ => panic!("Expected re-entrancy error"),
    }
}

// Including race recovery tests
#[test]
fn test_vacant_path_race_recovery() -> Result<()> {
    TEST_SKIP_CAPACITY_CHECK.store(true, Ordering::SeqCst);
    struct ResetGuard;
    impl Drop for ResetGuard {
        fn drop(&mut self) {
            TEST_SKIP_CAPACITY_CHECK.store(false, Ordering::SeqCst);
        }
    }
    let _reset = ResetGuard;

    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .initial_capacity(10)
        .build()?;

    for i in 0..10 {
        index.add(NodeId::new(i + 1).unwrap(), &[1.0, 0.0, 0.0, 0.0])?;
    }
    assert_eq!(index.len(), 10);

    index.add(NodeId::new(11).unwrap(), &[1.0, 0.0, 0.0, 0.0])?;

    assert_eq!(index.len(), 11);
    assert!(index.inner.read().capacity() > 10);

    Ok(())
}

#[test]
fn test_save_async_context() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("async_save.index");

    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build().unwrap();
    index
        .add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0])
        .unwrap();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    runtime.block_on(async {
        let result = index.save(&path);
        assert!(result.is_ok());
    });

    assert!(path.exists());
}

#[test]
fn test_load_mappings_bad_magic() -> Result<()> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test_index.usearch");
    let mappings_path = path.with_extension("usearch.mappings");

    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;
    index.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0])?;
    index.save(&path)?;

    let mut data = std::fs::read(&mappings_path).unwrap();
    data[0] = b'X';
    std::fs::write(&mappings_path, &data).unwrap();

    let result = HnswIndex::load(&path, HnswConfig::new(4, DistanceMetric::Cosine));
    assert!(result.is_err());
    match result {
        Err(Error::Vector(VectorError::IndexError(msg))) => {
            assert!(msg.contains("bad magic bytes"));
        }
        _ => panic!("Expected IndexError"),
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

    let mut data = std::fs::read(&mappings_path).unwrap();
    let count_offset = 15; // V2 offset
    let huge_count = (super::persistence::MAX_MAPPINGS_COUNT + 1) as u64;

    let count_bytes = huge_count.to_le_bytes();
    data[count_offset..count_offset + 8].copy_from_slice(&count_bytes);

    let crc_offset = data.len() - 4;
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&data[..crc_offset]);
    let new_crc = hasher.finalize();
    data[crc_offset..].copy_from_slice(&new_crc.to_le_bytes());

    std::fs::write(&mappings_path, &data).unwrap();

    let result = HnswIndex::load(&path, HnswConfig::new(4, DistanceMetric::Cosine));
    assert!(result.is_err());
    match result {
        Err(Error::Vector(VectorError::IndexError(msg))) => {
            assert!(msg.contains("exceeds maximum allowed"));
        }
        _ => panic!("Expected limit error"),
    }
    Ok(())
}
