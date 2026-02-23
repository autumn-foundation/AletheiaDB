use super::*;
use crate::index::vector::{DistanceMetric, Quantization, VectorIndex, StorageMode};
use crate::core::id::NodeId;
use crate::core::error::Result;
use std::sync::Arc;
use std::path::Path;
use super::utils::{TEST_RACE_HOOK, TEST_SKIP_CAPACITY_CHECK, FilterCallbackGuard, create_metric_wrapper, is_retryable_usearch_error};
use std::sync::atomic::Ordering;
use crc32fast::Hasher;
use std::io::Read;

fn create_test_index() -> HnswIndex {
    HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap()
}

#[test]
fn test_config_with_custom_metric() {
    let metric_fn = |_: &[f32], _: &[f32]| 0.0;
    let config = HnswConfig::new(4, DistanceMetric::Cosine)
        .with_custom_metric("test_metric", metric_fn);

    assert!(config.custom_metric.is_some());
    assert_eq!(config.custom_metric.unwrap().name, "test_metric");
}

#[test]
fn test_config_write_error() {
    let config = HnswConfig::default();
    let mut writer = MockFailWriter::new(0); // Fail immediately
    assert!(config.serialize_into(&mut writer).is_err());
}

#[test]
fn test_config_read_error() {
    let mut reader = MockFailReader::new(vec![], 0);
    assert!(HnswConfig::deserialize_from(&mut reader).is_err());
}

#[test]
fn test_add_batch() {
    let index = create_test_index();
    let items = vec![
        (NodeId::new(1).unwrap(), vec![1.0, 0.0, 0.0, 0.0]),
        (NodeId::new(2).unwrap(), vec![0.0, 1.0, 0.0, 0.0]),
    ];
    index.add_batch(&items).unwrap();
    assert_eq!(index.len(), 2);
}

#[test]
fn test_remove_batch() {
    let index = create_test_index();
    index.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0]).unwrap();
    index.add(NodeId::new(2).unwrap(), &[0.0, 1.0, 0.0, 0.0]).unwrap();

    let ids = vec![NodeId::new(1).unwrap()];
    index.remove_batch(&ids).unwrap();
    assert_eq!(index.len(), 1);
}

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
fn test_hnsw_config_deserialize_legacy() {
    let config = HnswConfig {
        dimensions: 128,
        metric: DistanceMetric::Cosine,
        m: 16,
        ef_construction: 128,
        ef_search: 64,
        capacity: 1000,
        quantization: Quantization::F32,
        storage: StorageMode::InMemory,
        custom_metric: None,
    };

    let mut buffer = Vec::new();
    buffer.extend_from_slice(&(config.dimensions as u64).to_le_bytes());
    buffer.push(config.metric.to_u8());
    buffer.extend_from_slice(&(config.m as u64).to_le_bytes());
    buffer.extend_from_slice(&(config.ef_construction as u64).to_le_bytes());
    buffer.extend_from_slice(&(config.ef_search as u64).to_le_bytes());
    buffer.extend_from_slice(&(config.capacity as u64).to_le_bytes());

    let mut cursor = std::io::Cursor::new(buffer);
    let deserialized = HnswConfig::deserialize_from(&mut cursor).unwrap();

    assert_eq!(config, deserialized);
    assert_eq!(deserialized.quantization, Quantization::F32);
}

#[test]
fn test_hnsw_config_deserialize_invalid_metric() {
    let mut buffer = Vec::new();
    buffer.extend_from_slice(&128u64.to_le_bytes());
    buffer.push(99);
    buffer.resize(100, 0);

    let mut cursor = std::io::Cursor::new(buffer);
    let result = HnswConfig::deserialize_from(&mut cursor);
    assert!(result.is_err());
}

#[test]
fn test_hnsw_config_deserialize_invalid_quantization() {
    let config = HnswConfig::default();
    let mut buffer = Vec::new();
    buffer.extend_from_slice(&(config.dimensions as u64).to_le_bytes());
    buffer.push(config.metric.to_u8());
    buffer.extend_from_slice(&(config.m as u64).to_le_bytes());
    buffer.extend_from_slice(&(config.ef_construction as u64).to_le_bytes());
    buffer.extend_from_slice(&(config.ef_search as u64).to_le_bytes());
    buffer.extend_from_slice(&(config.capacity as u64).to_le_bytes());
    buffer.push(99);

    let mut cursor = std::io::Cursor::new(buffer);
    let result = HnswConfig::deserialize_from(&mut cursor);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Invalid quantization"));
}

#[test]
fn test_builder_validation_limits() {
    let res = HnswIndexBuilder::new(10, DistanceMetric::Cosine).m(100).build();
    assert!(res.is_err());

    let res = HnswIndexBuilder::new(10, DistanceMetric::Cosine).m(0).build();
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
    assert!(result.unwrap_err().to_string().contains("only supported with F32"));
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
        assert!(results[i].1 >= results[i + 1].1);
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
    assert!((similarity - 1.0).abs() < 0.001);
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

    let results = index.search_with_filter(&[1.0, 0.0, 0.0, 0.0], 3, |id| id.as_u64() % 2 == 0)?;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, node2);
    Ok(())
}

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
fn test_validate_ef_parameters() {
    assert!(HnswIndexBuilder::new(4, DistanceMetric::Cosine).ef_construction(5).build().is_err());
    assert!(HnswIndexBuilder::new(4, DistanceMetric::Cosine).ef_construction(5000).build().is_err());
    assert!(HnswIndexBuilder::new(4, DistanceMetric::Cosine).ef_search(0).build().is_err());
    assert!(HnswIndexBuilder::new(4, DistanceMetric::Cosine).ef_search(5000).build().is_err());
}

#[test]
fn test_distance_to_similarity_conversion() -> Result<()> {
    let cosine_index = HnswIndexBuilder::new(3, DistanceMetric::Cosine).build()?;
    let n1 = NodeId::new(1).unwrap();
    let n2 = NodeId::new(2).unwrap();
    let n3 = NodeId::new(3).unwrap();

    cosine_index.add(n1, &[1.0, 0.0, 0.0])?;
    cosine_index.add(n2, &[0.9, 0.1, 0.0])?;
    cosine_index.add(n3, &[0.0, 1.0, 0.0])?;

    let results = cosine_index.search(&[1.0, 0.0, 0.0], 3)?;
    assert_eq!(results[0].0, n1);
    assert!(results[0].1 > 0.99);
    assert_eq!(results[1].0, n2);
    assert!(results[1].1 > 0.9);
    assert_eq!(results[2].0, n3);
    assert!(results[2].1 < 0.1 && results[2].1 > -0.1);
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
fn test_capacity_expansion_on_update() -> Result<()> {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .initial_capacity(2)
        .build()?;

    let node1 = NodeId::new(1).unwrap();
    let node2 = NodeId::new(2).unwrap();
    index.add(node1, &[1.0, 0.0, 0.0, 0.0])?;
    index.add(node2, &[0.0, 1.0, 0.0, 0.0])?;
    assert_eq!(index.len(), 2);

    index.add(node1, &[0.5, 0.5, 0.0, 0.0])?;
    assert_eq!(index.len(), 2);

    let node3 = NodeId::new(3).unwrap();
    index.add(node3, &[0.0, 0.0, 1.0, 0.0])?;
    assert_eq!(index.len(), 3);

    let node4 = NodeId::new(4).unwrap();
    index.add(node4, &[0.0, 0.0, 0.0, 1.0])?;
    assert_eq!(index.len(), 4);

    index.add(node2, &[0.2, 0.8, 0.0, 0.0])?;
    assert_eq!(index.len(), 4);

    let results = index.search(&[0.5, 0.5, 0.0, 0.0], 1)?;
    assert_eq!(results[0].0, node1);
    let results2 = index.search(&[0.2, 0.8, 0.0, 0.0], 1)?;
    assert_eq!(results2[0].0, node2);
    Ok(())
}

#[test]
fn test_concurrent_update_same_node() -> Result<()> {
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
fn test_concurrent_mixed_operations() -> Result<()> {
    use std::thread;
    let index = Arc::new(HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?);
    let num_threads = 8;
    let mut handles = vec![];

    for thread_id in 0..num_threads {
        let index_clone = Arc::clone(&index);
        let handle = thread::spawn(move || {
            let node = NodeId::new(thread_id as u64 + 1).unwrap();
            let vector = vec![thread_id as f32 / num_threads as f32, 0.0, 0.0, 0.0];
            index_clone.add(node, &vector).unwrap();

            for i in 0..5 {
                let val = (thread_id as f32 + i as f32) / (num_threads as f32 * 5.0);
                let updated_vector = vec![val, 1.0 - val, 0.0, 0.0];
                index_clone.add(node, &updated_vector).unwrap();
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }

    assert_eq!(index.len(), num_threads);
    let results = index.search(&[0.5, 0.5, 0.0, 0.0], num_threads)?;
    assert_eq!(results.len(), num_threads);
    Ok(())
}

#[test]
fn test_max_key_overflow_protection() -> Result<()> {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;
    const MAX_VALID_KEY: u64 = u64::MAX - 1000;
    index.next_key.store(MAX_VALID_KEY, std::sync::atomic::Ordering::SeqCst);

    let node1 = NodeId::new(1).unwrap();
    assert!(index.add(node1, &[1.0, 0.0, 0.0, 0.0]).is_ok());

    let node2 = NodeId::new(2).unwrap();
    let result = index.add(node2, &[0.0, 1.0, 0.0, 0.0]);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("overflow"));

    assert!(index.add(node1, &[0.5, 0.5, 0.0, 0.0]).is_ok());
    Ok(())
}

#[test]
fn test_update_nonexistent_then_exists() -> Result<()> {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;
    let node1 = NodeId::new(1).unwrap();

    index.add(node1, &[1.0, 0.0, 0.0, 0.0])?;
    assert_eq!(index.len(), 1);

    index.add(node1, &[0.0, 1.0, 0.0, 0.0])?;
    assert_eq!(index.len(), 1);

    let results = index.search(&[0.0, 1.0, 0.0, 0.0], 1)?;
    assert_eq!(results[0].0, node1);
    assert!(results[0].1 > 0.99);
    Ok(())
}

#[test]
fn test_stats_tracking() -> Result<()> {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;
    let node1 = NodeId::new(1).unwrap();
    let node2 = NodeId::new(2).unwrap();

    let initial_adds = index.stats.vectors_added.load(Ordering::Relaxed);
    index.add(node1, &[1.0, 0.0, 0.0, 0.0])?;
    index.add(node2, &[0.0, 1.0, 0.0, 0.0])?;

    let after_adds = index.stats.vectors_added.load(Ordering::Relaxed);
    assert_eq!(after_adds - initial_adds, 2);

    index.add(node1, &[0.5, 0.5, 0.0, 0.0])?;
    let after_update = index.stats.vectors_added.load(Ordering::Relaxed);
    assert_eq!(after_update - initial_adds, 3);
    Ok(())
}

#[test]
fn test_metric_wrapper_safe_on_null() {
    let distance_fn = Arc::new(|_: &[f32], _: &[f32]| 0.0);
    let wrapper = create_metric_wrapper(4, distance_fn);
    let vec = [0.0f32; 4];
    let result = wrapper(vec.as_ptr(), std::ptr::null());
    assert_eq!(result, f32::MAX);
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
    assert!(result.unwrap_err().to_string().contains("bad magic bytes"));
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

    let mut data = std::fs::read(&mappings_path).unwrap();
    data[4] = 99;
    std::fs::write(&mappings_path, &data).unwrap();

    let result = HnswIndex::load(&path, HnswConfig::new(4, DistanceMetric::Cosine));
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Unsupported mapping file version"));
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

    let mut data = std::fs::read(&mappings_path).unwrap();
    let len = data.len();
    data[len - 5] = data[len - 5].wrapping_add(1);
    std::fs::write(&mappings_path, &data).unwrap();

    let result = HnswIndex::load(&path, HnswConfig::new(4, DistanceMetric::Cosine));
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("CRC mismatch"));
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

    let data = std::fs::read(&mappings_path).unwrap();
    std::fs::write(&mappings_path, &data[..10]).unwrap();

    let result = HnswIndex::load(&path, HnswConfig::new(4, DistanceMetric::Cosine));
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("too small"));
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

    let mut data = std::fs::read(&mappings_path).unwrap();
    let count_offset = 15;
    data[count_offset] = 2; // Increase count

    let crc_offset = data.len() - 4;
    let mut hasher = Hasher::new();
    hasher.update(&data[..crc_offset]);
    let new_crc = hasher.finalize();
    data[crc_offset..].copy_from_slice(&new_crc.to_le_bytes());

    std::fs::write(&mappings_path, &data).unwrap();

    let result = HnswIndex::load(&path, HnswConfig::new(4, DistanceMetric::Cosine));
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("size mismatch"));
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

    let mut data = std::fs::read(&mappings_path).unwrap();
    let count_offset = 15;
    data[count_offset..count_offset + 8].copy_from_slice(&u64::MAX.to_le_bytes());

    let crc_offset = data.len() - 4;
    let mut hasher = Hasher::new();
    hasher.update(&data[..crc_offset]);
    let new_crc = hasher.finalize();
    data[crc_offset..].copy_from_slice(&new_crc.to_le_bytes());

    std::fs::write(&mappings_path, &data).unwrap();

    let result = HnswIndex::load(&path, HnswConfig::new(4, DistanceMetric::Cosine));
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("overflow") || msg.contains("exceeds maximum allowed"));
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
    let count_offset = 15;
    let huge_count = (super::persistence::MAX_MAPPINGS_COUNT + 1) as u64;
    data[count_offset..count_offset + 8].copy_from_slice(&huge_count.to_le_bytes());

    let crc_offset = data.len() - 4;
    let mut hasher = Hasher::new();
    hasher.update(&data[..crc_offset]);
    let new_crc = hasher.finalize();
    data[crc_offset..].copy_from_slice(&new_crc.to_le_bytes());

    std::fs::write(&mappings_path, &data).unwrap();

    let result = HnswIndex::load(&path, HnswConfig::new(4, DistanceMetric::Cosine));
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("exceeds maximum allowed"));
    Ok(())
}

#[test]
fn test_save_mappings_large_streaming() -> Result<()> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test_streaming.usearch");
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;

    for i in 1..=2000 {
        index.add(NodeId::new(i).unwrap(), &[1.0, 0.0, 0.0, 0.0])?;
    }
    index.save(&path)?;

    let loaded = HnswIndex::load(&path, HnswConfig::new(4, DistanceMetric::Cosine))?;
    assert_eq!(loaded.len(), 2000);
    assert!(!loaded.search(&[1.0, 0.0, 0.0, 0.0], 1)?.is_empty());
    Ok(())
}

struct MockFailWriter {
    fail_after: usize,
    written: usize,
    buffer: Vec<u8>,
}

impl MockFailWriter {
    fn new(fail_after: usize) -> Self {
        Self { fail_after, written: 0, buffer: Vec::new() }
    }
}

impl std::io::Write for MockFailWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.written + buf.len() > self.fail_after {
            return Err(std::io::Error::other("Mock write error"));
        }
        self.written += buf.len();
        self.buffer.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
}

#[test]
fn test_save_mappings_write_errors() {
    use super::persistence::write_mappings_to_writer;
    let mappings = [(NodeId::new(1).unwrap(), 100), (NodeId::new(2).unwrap(), 200)];
    let config = HnswConfig::default();

    let mut writer = MockFailWriter::new(3);
    assert!(write_mappings_to_writer(&mut writer, mappings.iter().copied(), mappings.len(), &config).is_err());

    let mut writer = MockFailWriter::new(23 + 16 + 1);
    assert!(write_mappings_to_writer(&mut writer, mappings.iter().copied(), mappings.len(), &config).is_err());

    let mut writer = MockFailWriter::new(55 + 1);
    assert!(write_mappings_to_writer(&mut writer, mappings.iter().copied(), mappings.len(), &config).is_err());
}

struct MockFlushFailWriter;
impl std::io::Write for MockFlushFailWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> { Ok(buf.len()) }
    fn flush(&mut self) -> std::io::Result<()> { Err(std::io::Error::other("Mock flush error")) }
}

#[test]
fn test_save_mappings_flush_error() {
    use super::persistence::write_mappings_to_writer;
    let mappings: [(NodeId, u64); 0] = [];
    let config = HnswConfig::default();
    let mut writer = MockFlushFailWriter;
    assert!(write_mappings_to_writer(&mut writer, mappings.iter().copied(), mappings.len(), &config).is_err());
}

#[test]
fn test_save_mappings_file_create_error() {
    let dir = tempfile::tempdir().unwrap();
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build().unwrap();
    let index_path = dir.path().join("test.index");
    let mappings_path = index_path.with_extension("usearch.mappings");
    std::fs::create_dir(&mappings_path).unwrap();

    let result = index.save(&index_path);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Failed to create mappings file"));
}

#[test]
fn test_custom_metric_execution_coverage() {
    let metric_fn = |a: &[f32], b: &[f32]| -> f32 {
        a.iter().zip(b.iter()).map(|(x, y)| (x - y).abs()).sum()
    };
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .quantization(Quantization::F32)
        .with_custom_metric("manhattan", metric_fn)
        .build().unwrap();

    for i in 0..10 {
        let id = NodeId::new(i + 1).unwrap();
        let vec = if i % 2 == 0 { [1.0, 0.0, 0.0, 0.0] } else { [0.0, 1.0, 0.0, 0.0] };
        index.add(id, &vec).unwrap();
    }
    assert_eq!(index.search(&[0.9, 0.1, 0.0, 0.0], 5).unwrap().len(), 5);
}

#[test]
fn test_add_race_retry_value_change_coverage() {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build().unwrap();
    let node = NodeId::new(1).unwrap();
    index.add(node, &[1.0, 0.0, 0.0, 0.0]).unwrap();

    TEST_RACE_HOOK.with(|h| {
        h.set(Some(|idx, node_id| {
            idx.id_mapping.insert(node_id, 999);
            idx.reverse_mapping.insert(999, node_id);
        }))
    });

    let result = index.add(node, &[0.0, 1.0, 0.0, 0.0]);
    TEST_RACE_HOOK.with(|h| h.set(None));
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("mapping changed"));
}

#[test]
fn test_add_race_retry_removal_coverage() {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build().unwrap();
    let node = NodeId::new(2).unwrap();
    index.add(node, &[1.0, 0.0, 0.0, 0.0]).unwrap();

    TEST_RACE_HOOK.with(|h| {
        h.set(Some(|idx, node_id| {
            idx.id_mapping.remove(&node_id);
        }))
    });

    let result = index.add(node, &[0.0, 1.0, 0.0, 0.0]);
    TEST_RACE_HOOK.with(|h| h.set(None));
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("node removed"));
}

#[test]
fn test_add_race_vacant_coverage() {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build().unwrap();
    let node = NodeId::new(3).unwrap();

    TEST_RACE_HOOK.with(|h| {
        h.set(Some(|idx, node_id| {
            idx.id_mapping.insert(node_id, 999);
        }))
    });

    let result = index.add(node, &[0.5, 0.5, 0.5, 0.5]);
    TEST_RACE_HOOK.with(|h| h.set(None));
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Concurrent add detected"));
    assert_eq!(index.len(), 0);
}

#[test]
fn test_save_coverage() -> Result<()> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("coverage.index");
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;
    index.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0])?;
    index.save(&path)?;
    assert!(path.exists());
    Ok(())
}

#[test]
fn test_hnsw_index_debug() {
    let index = create_test_index();
    let debug_str = format!("{:?}", index);
    assert!(debug_str.contains("HnswIndex"));
    assert!(debug_str.contains("config"));
    assert!(debug_str.contains("is_mmap"));
}

struct MockFailReader {
    data: Vec<u8>,
    fail_at: usize,
    cursor: usize,
}

impl MockFailReader {
    fn new(data: Vec<u8>, fail_at: usize) -> Self {
        Self { data, fail_at, cursor: 0 }
    }
}

impl Read for MockFailReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.cursor >= self.fail_at {
            return Err(std::io::Error::other("Mock read error"));
        }
        let remaining = self.fail_at - self.cursor;
        let available = self.data.len() - self.cursor;
        let to_read = buf.len().min(remaining).min(available);
        if to_read == 0 { return Ok(0); }
        buf[..to_read].copy_from_slice(&self.data[self.cursor..self.cursor + to_read]);
        self.cursor += to_read;
        Ok(to_read)
    }
}

#[test]
fn test_deserialize_config_failures_detailed() {
    let config = HnswConfig::default();
    let mut buffer = Vec::new();
    buffer.extend_from_slice(&(config.dimensions as u64).to_le_bytes());
    buffer.push(config.metric.to_u8());
    buffer.extend_from_slice(&(config.m as u64).to_le_bytes());
    buffer.extend_from_slice(&(config.ef_construction as u64).to_le_bytes());
    buffer.extend_from_slice(&(config.ef_search as u64).to_le_bytes());
    buffer.extend_from_slice(&(config.capacity as u64).to_le_bytes());
    buffer.push(config.quantization.to_u8());

    let mut reader = MockFailReader::new(buffer.clone(), 9 + 4);
    assert!(HnswConfig::deserialize_from(&mut reader).is_err());

    let mut reader = MockFailReader::new(buffer.clone(), 17 + 4);
    assert!(HnswConfig::deserialize_from(&mut reader).is_err());

    let mut reader = MockFailReader::new(buffer.clone(), 25 + 4);
    assert!(HnswConfig::deserialize_from(&mut reader).is_err());

    let mut reader = MockFailReader::new(buffer.clone(), 33 + 4);
    assert!(HnswConfig::deserialize_from(&mut reader).is_err());
}

#[test]
fn test_load_mappings_file_open_error() {
    let _path = Path::new("/path/does/not/exist/test.mappings");
    let dir = tempfile::tempdir().unwrap();
    let _dir_path = dir.path().to_path_buf();
}

#[test]
fn test_load_mappings_read_failures() {
}

#[test]
fn test_add_reentrancy_check() {
    let index = create_test_index();
    let node_id = NodeId::new(1).unwrap();
    let vec = vec![1.0, 0.0, 0.0, 0.0];
    let _guard = FilterCallbackGuard::new();
    let result = index.add(node_id, &vec);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Cannot modify index from within a search_with_filter callback"));
}

#[test]
fn test_remove_reentrancy_check() {
    let index = create_test_index();
    let node_id = NodeId::new(1).unwrap();
    let _guard = FilterCallbackGuard::new();
    let result = index.remove(node_id);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Cannot modify index from within a search_with_filter callback"));
}

#[test]
fn test_save_reentrancy_check() {
    let index = create_test_index();
    let path = Path::new("dummy.index");
    let _guard = FilterCallbackGuard::new();
    let result = index.save(path);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Cannot save index from within a search_with_filter callback"));
}

#[test]
fn test_search_reentrancy_check() {
    let index = create_test_index();
    let query = vec![1.0, 0.0, 0.0, 0.0];
    let _guard = FilterCallbackGuard::new();
    let result = index.search(&query, 10);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Cannot perform search from within a search_with_filter callback"));
}

#[test]
fn test_search_with_filter_reentrancy_check() {
    let index = create_test_index();
    let query = vec![1.0, 0.0, 0.0, 0.0];
    let _guard = FilterCallbackGuard::new();
    let result = index.search_with_filter(&query, 10, |_| true);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Cannot perform search_with_filter from within a search_with_filter callback"));
}

#[test]
fn test_metric_wrapper_panic_resilience() {
    let distance_fn = Arc::new(|_: &[f32], _: &[f32]| -> f32 { panic!("Test panic"); });
    let wrapper = create_metric_wrapper(4, distance_fn);
    let data = [0.0f32; 4];
    let result = wrapper(data.as_ptr(), data.as_ptr());
    assert_eq!(result, f32::MAX);
}

#[test]
fn test_metric_wrapper_success_direct() {
    let distance_fn = Arc::new(|a: &[f32], b: &[f32]| -> f32 {
        a.iter().zip(b.iter()).map(|(x, y)| (x - y).abs()).sum()
    });
    let wrapper = create_metric_wrapper(4, distance_fn);
    let data_a = [1.0f32, 2.0, 3.0, 4.0];
    let data_b = [1.5f32, 2.5, 3.5, 4.5];
    let result = wrapper(data_a.as_ptr(), data_b.as_ptr());
    assert!((result - 2.0).abs() < f32::EPSILON);
}

#[test]
fn test_capacity_check_and_expand() {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).initial_capacity(10).build().unwrap();
    assert_eq!(index.len(), 0);
    index.check_and_expand_capacity(1).unwrap();
    for i in 0..10 {
        let id = NodeId::new(i + 1).unwrap();
        index.add(id, &[1.0, 0.0, 0.0, 0.0]).unwrap();
    }
    assert_eq!(index.len(), 10);
    index.check_and_expand_capacity(1).unwrap();
}

#[test]
fn test_vacant_path_race_recovery() -> Result<()> {
    TEST_SKIP_CAPACITY_CHECK.store(true, Ordering::SeqCst);
    struct ResetGuard;
    impl Drop for ResetGuard { fn drop(&mut self) { TEST_SKIP_CAPACITY_CHECK.store(false, Ordering::SeqCst); } }
    let _reset = ResetGuard;

    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).initial_capacity(10).build()?;
    for i in 0..10 {
        index.add(NodeId::new(i + 1).unwrap(), &[1.0, 0.0, 0.0, 0.0])?;
    }
    index.add(NodeId::new(11).unwrap(), &[1.0, 0.0, 0.0, 0.0])?;
    assert_eq!(index.len(), 11);
    assert!(index.inner.read().capacity() > 10);
    Ok(())
}

#[test]
fn test_occupied_path_inconsistency_race_recovery() -> Result<()> {
    TEST_SKIP_CAPACITY_CHECK.store(true, Ordering::SeqCst);
    struct ResetGuard;
    impl Drop for ResetGuard { fn drop(&mut self) { TEST_SKIP_CAPACITY_CHECK.store(false, Ordering::SeqCst); } }
    let _reset = ResetGuard;

    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).initial_capacity(10).build()?;
    for i in 0..10 {
        index.add(NodeId::new(i + 1).unwrap(), &[1.0, 0.0, 0.0, 0.0])?;
    }
    let node_id = NodeId::new(1).unwrap();

    TEST_RACE_HOOK.with(|h| {
        h.set(Some(|idx, _id| {
            let index = idx.inner.write();
            let _ = index.remove(0);
            let _ = index.add(999, &[0.0, 1.0, 0.0, 0.0]);
        }))
    });

    index.add(node_id, &[0.0, 1.0, 0.0, 0.0])?;
    TEST_RACE_HOOK.with(|h| h.set(None));
    assert_eq!(index.len(), 11);
    assert!(index.inner.read().capacity() > 10);
    Ok(())
}
