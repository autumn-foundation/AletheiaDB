use super::*;
use crate::core::id::NodeId;
use crate::index::vector::{DistanceMetric, Quantization, StorageMode};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;

// Helper to access persistence functions which are pub(crate)
use super::persistence::IndexMetadata;

#[cfg(test)]
mod sentry_tests {
    use super::*;

    #[test]
    fn test_metric_wrapper_safe_on_unaligned() {
        let distance_fn = Arc::new(|_: &[f32], _: &[f32]| 0.0);
        let wrapper = create_metric_wrapper(4, distance_fn);

        // Create a buffer and get an unaligned pointer
        let buffer = [0u8; 32];
        // Address + 1 is definitely unaligned for f32 (align 4)
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
}

#[cfg(test)]
mod functional_tests {
    use super::*;

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

        // Add random vectors
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
        // Test DotProduct similarity conversion
        // Dot Product of (1,0) and (1,0) is 1.0.
        // usearch IP metric returns 1 - dot_product (or similar).
        // We verify that our conversion (1.0 - distance) yields the correct dot product (1.0).

        let index = HnswIndexBuilder::new(2, DistanceMetric::DotProduct).build()?;
        let node = NodeId::new(1).unwrap();
        index.add(node, &[1.0, 0.0])?;

        let results = index.search(&[1.0, 0.0], 1)?;
        assert_eq!(results.len(), 1);
        let similarity = results[0].1;

        // We expect similarity to be exactly 1.0 for normalized dot product of identical unit vectors.
        // Previously this was returning 0.0 (or -0.0) due to incorrect conversion.
        assert!(
            (similarity - 1.0).abs() < 0.001,
            "Expected 1.0, got {}",
            similarity
        );

        Ok(())
    }

    #[test]
    fn test_metric_wrapper_safe_on_unaligned() {
        // This test ensures that the metric wrapper correctly detects unaligned pointers.
        let distance_fn = Arc::new(|_: &[f32], _: &[f32]| 0.0);
        let wrapper = create_metric_wrapper(4, distance_fn);

        // Create a buffer that we can misalign
        // We need at least 4 f32s (16 bytes) + 1 byte offset
        let mut buffer = vec![0u8; 16 + 8];

        // Get an aligned pointer
        let aligned_ptr = buffer.as_mut_ptr();

        // Create an unaligned pointer by adding 1 byte offset
        // SAFETY: We allocated enough space. This pointer is valid but unaligned for f32.
        let unaligned_ptr = unsafe { aligned_ptr.add(1) } as *const f32;
        let valid_ptr = aligned_ptr as *const f32;

        // Pass unaligned pointer - should return f32::MAX
        let result = wrapper(valid_ptr, unaligned_ptr);
        assert_eq!(result, f32::MAX);
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
        // Should only return node2 (node1 is deleted)
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

        // Filter to only even node IDs
        let results =
            index.search_with_filter(&[1.0, 0.0, 0.0, 0.0], 3, |id| id.as_u64() % 2 == 0)?;

        // Should only return node2
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

    #[test]
    fn test_cosine_similarity_nan_handling() {
        // Create index with Cosine metric
        let index = HnswIndexBuilder::new(1, DistanceMetric::Cosine)
            .build()
            .unwrap();

        // Populate reverse mapping manually so convert_matches can resolve IDs
        let id1 = NodeId::new(1).unwrap();
        let id2 = NodeId::new(2).unwrap();
        index.reverse_mapping.insert(1, id1);
        index.reverse_mapping.insert(2, id2);

        // Manually construct matches with NaN and Inf
        let matches = Matches {
            keys: vec![1, 2],
            distances: vec![f32::NAN, f32::INFINITY],
        };

        // Convert matches (this is private but available in tests module due to super::*)
        let results = index.convert_matches(matches);

        // Verify NaN -> 0.0
        assert_eq!(results[0].0, id1);
        assert_eq!(results[0].1, 0.0);

        // Verify Inf -> -1.0 (clamped)
        assert_eq!(results[1].0, id2);
        assert_eq!(results[1].1, -1.0);
    }

    #[test]
    fn test_distance_to_similarity_conversion() -> Result<()> {
        // Test Cosine similarity conversion
        let cosine_index = HnswIndexBuilder::new(3, DistanceMetric::Cosine).build()?;

        let n1 = NodeId::new(1).unwrap();
        let n2 = NodeId::new(2).unwrap();
        let n3 = NodeId::new(3).unwrap();

        cosine_index.add(n1, &[1.0, 0.0, 0.0])?; // Identical to query
        cosine_index.add(n2, &[0.9, 0.1, 0.0])?; // Very similar
        cosine_index.add(n3, &[0.0, 1.0, 0.0])?; // Orthogonal

        let results = cosine_index.search(&[1.0, 0.0, 0.0], 3)?;

        // Verify similarity values (not distances)
        assert_eq!(results[0].0, n1);
        assert!(results[0].1 > 0.99); // Identical: similarity ~= 1.0

        assert_eq!(results[1].0, n2);
        assert!(results[1].1 > 0.9); // Very similar: similarity > 0.9

        assert_eq!(results[2].0, n3);
        assert!(results[2].1 < 0.1 && results[2].1 > -0.1); // Orthogonal: similarity ~= 0.0

        Ok(())
    }

    #[test]
    fn test_update_existing_node() -> Result<()> {
        // Test the Occupied entry path in add() - updating an existing node
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;

        let node1 = NodeId::new(1).unwrap();

        // Add initial vector
        index.add(node1, &[1.0, 0.0, 0.0, 0.0])?;
        assert_eq!(index.len(), 1);

        // Update with new vector (this exercises the Occupied entry path)
        index.add(node1, &[0.0, 1.0, 0.0, 0.0])?;
        assert_eq!(index.len(), 1); // Still only one node

        // Verify the vector was updated, not duplicated
        let results = index.search(&[0.0, 1.0, 0.0, 0.0], 1)?;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, node1);
        assert!(results[0].1 > 0.99); // Should match the new vector

        Ok(())
    }

    #[test]
    fn test_capacity_expansion_on_add() -> Result<()> {
        // Test capacity expansion during initial adds (Vacant entry path)
        // Start with small initial capacity to force expansion
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .initial_capacity(2) // Start with small capacity
            .build()?;

        // Add first two nodes - should fit in initial capacity
        let node1 = NodeId::new(1).unwrap();
        let node2 = NodeId::new(2).unwrap();
        index.add(node1, &[1.0, 0.0, 0.0, 0.0])?;
        index.add(node2, &[0.0, 1.0, 0.0, 0.0])?;
        assert_eq!(index.len(), 2);

        // Add third node - should trigger capacity expansion code path
        let node3 = NodeId::new(3).unwrap();
        index.add(node3, &[0.0, 0.0, 1.0, 0.0])?;
        assert_eq!(index.len(), 3);

        // Add more nodes to verify expansion worked
        let node4 = NodeId::new(4).unwrap();
        let node5 = NodeId::new(5).unwrap();
        index.add(node4, &[0.0, 0.0, 0.0, 1.0])?;
        index.add(node5, &[0.5, 0.5, 0.0, 0.0])?;
        assert_eq!(index.len(), 5);

        // Verify all nodes are searchable
        let results = index.search(&[1.0, 0.0, 0.0, 0.0], 5)?;
        assert_eq!(results.len(), 5);

        Ok(())
    }

    #[test]
    fn test_capacity_expansion_on_update() -> Result<()> {
        // Test capacity expansion during updates (Occupied entry path with expansion)
        // Start with small capacity to test expansion in update path
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .initial_capacity(2)
            .build()?;

        let node1 = NodeId::new(1).unwrap();
        let node2 = NodeId::new(2).unwrap();

        // Fill to initial capacity
        index.add(node1, &[1.0, 0.0, 0.0, 0.0])?;
        index.add(node2, &[0.0, 1.0, 0.0, 0.0])?;
        assert_eq!(index.len(), 2);

        // Update node1 multiple times (exercises Occupied path)
        index.add(node1, &[0.5, 0.5, 0.0, 0.0])?;
        assert_eq!(index.len(), 2); // Still 2 nodes

        // Add new nodes to trigger and test expansion
        let node3 = NodeId::new(3).unwrap();
        index.add(node3, &[0.0, 0.0, 1.0, 0.0])?;
        assert_eq!(index.len(), 3);

        let node4 = NodeId::new(4).unwrap();
        index.add(node4, &[0.0, 0.0, 0.0, 1.0])?;
        assert_eq!(index.len(), 4);

        // Update again after expansion to test Occupied path with larger capacity
        index.add(node2, &[0.2, 0.8, 0.0, 0.0])?;
        assert_eq!(index.len(), 4); // Still 4 nodes

        // Verify updates worked correctly
        let results = index.search(&[0.5, 0.5, 0.0, 0.0], 1)?;
        assert_eq!(results[0].0, node1);

        let results2 = index.search(&[0.2, 0.8, 0.0, 0.0], 1)?;
        assert_eq!(results2[0].0, node2);

        Ok(())
    }

    #[test]
    fn test_concurrent_update_same_node() -> Result<()> {
        use std::sync::Arc;
        use std::thread;

        // Test the race condition fix - multiple threads updating the same node
        let index = Arc::new(HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?);
        let node1 = NodeId::new(1).unwrap();

        // Add initial vector
        index.add(node1, &[1.0, 0.0, 0.0, 0.0])?;

        let num_threads = 10;
        let updates_per_thread = 10;

        let mut handles = vec![];

        for thread_id in 0..num_threads {
            let index_clone = Arc::clone(&index);
            let handle = thread::spawn(move || {
                for i in 0..updates_per_thread {
                    // Each thread updates the same node with different vectors
                    let val = (thread_id * updates_per_thread + i) as f32 / 100.0;
                    let vector = vec![val, 1.0 - val, 0.0, 0.0];
                    index_clone.add(node1, &vector).unwrap();
                }
            });
            handles.push(handle);
        }

        // Wait for all threads
        for handle in handles {
            handle.join().unwrap();
        }

        // Should still be exactly one node, not duplicates
        assert_eq!(index.len(), 1);

        // Verify the node is still searchable
        let results = index.search(&[0.5, 0.5, 0.0, 0.0], 1)?;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, node1);

        Ok(())
    }

    #[test]
    fn test_concurrent_mixed_operations() -> Result<()> {
        use std::sync::Arc;
        use std::thread;

        // Test concurrent adds and updates to different nodes
        let index = Arc::new(HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?);

        let num_threads = 8;
        let mut handles = vec![];

        for thread_id in 0..num_threads {
            let index_clone = Arc::clone(&index);
            let handle = thread::spawn(move || {
                // Each thread works with its own node
                let node = NodeId::new(thread_id as u64 + 1).unwrap();

                // Add the node
                let vector = vec![thread_id as f32 / num_threads as f32, 0.0, 0.0, 0.0];
                index_clone.add(node, &vector).unwrap();

                // Update it multiple times
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

        // Should have exactly num_threads nodes
        assert_eq!(index.len(), num_threads);

        // All nodes should be searchable
        let results = index.search(&[0.5, 0.5, 0.0, 0.0], num_threads)?;
        assert_eq!(results.len(), num_threads);

        Ok(())
    }

    #[test]
    fn test_max_key_overflow_protection() -> Result<()> {
        // Test that we reject IDs that would exceed MAX_VALID_KEY
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;

        // Manually set next_key to exactly at the limit
        const MAX_VALID_KEY: u64 = u64::MAX - 1000;
        index
            .next_key
            .store(MAX_VALID_KEY, std::sync::atomic::Ordering::SeqCst);

        // This should succeed (uses MAX_VALID_KEY, then increments to MAX_VALID_KEY+1)
        let node1 = NodeId::new(1).unwrap();
        assert!(index.add(node1, &[1.0, 0.0, 0.0, 0.0]).is_ok());

        // Now next_key = MAX_VALID_KEY+1, which is > MAX_VALID_KEY, so this should fail
        let node2 = NodeId::new(2).unwrap();
        let result = index.add(node2, &[0.0, 1.0, 0.0, 0.0]);
        assert!(result.is_err());

        if let Err(Error::Vector(VectorError::IndexError(msg))) = result {
            assert!(msg.contains("overflow") || msg.contains("exceeded"));
        } else {
            panic!(
                "Expected IndexError with overflow/exceeded message, got: {:?}",
                result
            );
        }

        // Updating existing node should still work (doesn't allocate new key)
        assert!(index.add(node1, &[0.5, 0.5, 0.0, 0.0]).is_ok());

        Ok(())
    }

    #[test]
    fn test_update_nonexistent_then_exists() -> Result<()> {
        // Test edge case: try to "update" a node that doesn't exist, then add it properly
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;

        let node1 = NodeId::new(1).unwrap();

        // First add creates the node
        index.add(node1, &[1.0, 0.0, 0.0, 0.0])?;
        assert_eq!(index.len(), 1);

        // Second add updates it
        index.add(node1, &[0.0, 1.0, 0.0, 0.0])?;
        assert_eq!(index.len(), 1);

        // Verify it has the updated vector
        let results = index.search(&[0.0, 1.0, 0.0, 0.0], 1)?;
        assert_eq!(results[0].0, node1);
        assert!(results[0].1 > 0.99);

        Ok(())
    }

    #[test]
    fn test_stats_tracking() -> Result<()> {
        // Test that statistics are correctly tracked for adds and updates
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;

        let node1 = NodeId::new(1).unwrap();
        let node2 = NodeId::new(2).unwrap();

        let initial_adds = index
            .stats
            .vectors_added
            .load(std::sync::atomic::Ordering::Relaxed);

        // Add two nodes
        index.add(node1, &[1.0, 0.0, 0.0, 0.0])?;
        index.add(node2, &[0.0, 1.0, 0.0, 0.0])?;

        let after_adds = index
            .stats
            .vectors_added
            .load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(after_adds - initial_adds, 2);

        // Update node1 - should still increment vectors_added counter
        index.add(node1, &[0.5, 0.5, 0.0, 0.0])?;

        let after_update = index
            .stats
            .vectors_added
            .load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(after_update - initial_adds, 3);

        Ok(())
    }

    #[test]
    fn test_save_coverage() -> Result<()> {
        // Basic save test to ensure save_internal lines are covered
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("coverage.index");

        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;
        index.add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0])?;

        // This triggers save_internal
        index.save(&path)?;

        assert!(path.exists());
        Ok(())
    }
}

#[cfg(test)]
mod warden_tests {
    use super::*;
    use crate::core::property::MAX_VECTOR_DIMENSIONS;

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

    #[test]
    fn test_validate_metadata_dimensions_too_large() {
        let huge_dims = MAX_VECTOR_DIMENSIONS + 1;
        let metadata = Some(IndexMetadata {
            dimensions: huge_dims,
            quantization: Quantization::F32,
            metric: DistanceMetric::Cosine,
        });
        let config = HnswConfig::default();

        let result = super::persistence::validate_metadata(metadata, &config);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("Stored index dimensions"));
        assert!(msg.contains("exceeds maximum allowed"));
    }

    #[test]
    fn test_load_dimensions_too_large_in_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.index");

        // Create a config with manually set huge dimensions (bypassing builder)
        let config = HnswConfig {
            dimensions: MAX_VECTOR_DIMENSIONS + 1,
            ..Default::default()
        };

        // Attempt to load (file existence doesn't matter as config check is first)
        let result = HnswIndex::load(&path, config);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("dimensions"));
        assert!(msg.contains("exceeds maximum allowed"));
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;

    #[test]
    fn test_metric_wrapper_null_pointer() {
        let distance_fn = Arc::new(|_: &[f32], _: &[f32]| 0.0);
        let wrapper = create_metric_wrapper(4, distance_fn);

        let null_ptr: *const f32 = std::ptr::null();
        let valid_data = [0.0f32; 4];
        let valid_ptr = valid_data.as_ptr();

        // This should return f32::MAX
        let result = wrapper(null_ptr, valid_ptr);
        assert_eq!(result, f32::MAX);
    }

    #[test]
    fn test_metric_wrapper_unaligned_pointer() {
        let distance_fn = Arc::new(|_: &[f32], _: &[f32]| 0.0);
        let wrapper = create_metric_wrapper(4, distance_fn);

        let data = [0u8; 32];
        let unaligned_ptr = unsafe { data.as_ptr().add(1) as *const f32 };
        let valid_data = [0.0f32; 4];
        let valid_ptr = valid_data.as_ptr();

        // This should return f32::MAX
        let result = wrapper(unaligned_ptr, valid_ptr);
        assert_eq!(result, f32::MAX);
    }

    #[test]
    fn test_filter_callback_guard_reset() {
        // Ensure flag is initially false
        IN_FILTER_CALLBACK.with(|flag| flag.set(false));

        {
            let _guard = FilterCallbackGuard::new();
            // Verify flag is set to true
            assert!(IN_FILTER_CALLBACK.with(|flag| flag.get()));
        }

        // Verify flag is reset to false after drop
        assert!(!IN_FILTER_CALLBACK.with(|flag| flag.get()));
    }

    #[test]
    fn test_filter_callback_guard_manual_drop() {
        IN_FILTER_CALLBACK.with(|flag| flag.set(false));

        let guard = FilterCallbackGuard::new();
        assert!(IN_FILTER_CALLBACK.with(|flag| flag.get()));

        drop(guard);
        assert!(!IN_FILTER_CALLBACK.with(|flag| flag.get()));
    }

    #[test]
    fn test_metric_wrapper_panic_resilience() {
        // Ensure that a panicking metric function doesn't crash the process
        // but returns f32::MAX instead.
        let distance_fn = Arc::new(|_: &[f32], _: &[f32]| -> f32 {
            panic!("Test panic");
        });
        let wrapper = create_metric_wrapper(4, distance_fn);

        let data = [0.0f32; 4];
        let ptr = data.as_ptr();

        // This should NOT panic
        let result = wrapper(ptr, ptr);

        // Should return max distance
        assert_eq!(result, f32::MAX);
    }

    #[test]
    fn test_metric_wrapper_success_direct() {
        // Ensure that a non-panicking metric function works correctly.
        // This provides direct coverage of the Ok path in catch_unwind.
        let distance_fn = Arc::new(|a: &[f32], b: &[f32]| -> f32 {
            a.iter().zip(b.iter()).map(|(x, y)| (x - y).abs()).sum()
        });
        let wrapper = create_metric_wrapper(4, distance_fn);

        let data_a = [1.0f32, 2.0, 3.0, 4.0];
        let data_b = [1.5f32, 2.5, 3.5, 4.5];

        let result = wrapper(data_a.as_ptr(), data_b.as_ptr());

        // |1.0-1.5| + |2.0-2.5| + |3.0-3.5| + |4.0-4.5| = 0.5 * 4 = 2.0
        assert!((result - 2.0).abs() < f32::EPSILON);
    }
}

#[cfg(test)]
mod capacity_tests {
    use super::*;

    #[test]
    fn test_capacity_check_and_expand() {
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .initial_capacity(10)
            .build()
            .unwrap();

        // Initially size 0, capacity 10
        assert_eq!(index.len(), 0);

        // This should pass without expanding
        index.check_and_expand_capacity(1).unwrap();

        // Fill to capacity
        for i in 0..10 {
            let id = NodeId::new(i + 1).unwrap();
            index.add(id, &[1.0, 0.0, 0.0, 0.0]).unwrap();
        }

        assert_eq!(index.len(), 10);

        // This should trigger expansion
        index.check_and_expand_capacity(1).unwrap();
    }
}

#[cfg(test)]
mod race_recovery_tests {
    use super::*;

    #[test]
    fn test_vacant_path_race_recovery() -> Result<()> {
        // Setup: Ensure we skip the optimistic check
        TEST_SKIP_CAPACITY_CHECK.store(true, Ordering::SeqCst);
        struct ResetGuard;
        impl Drop for ResetGuard {
            fn drop(&mut self) {
                TEST_SKIP_CAPACITY_CHECK.store(false, Ordering::SeqCst);
            }
        }
        let _reset = ResetGuard;

        // Create index with small capacity
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .initial_capacity(10)
            .build()?;

        // Fill to capacity
        for i in 0..10 {
            index.add(NodeId::new(i + 1).unwrap(), &[1.0, 0.0, 0.0, 0.0])?;
        }
        assert_eq!(index.len(), 10);

        // At this point, size=10, capacity=10.
        // optimistic check is skipped.
        // add(11) will enter Vacant path.
        // It will acquire inner write lock.
        // It will check size >= capacity (10 >= 10). True.
        // It should trigger expansion.

        index.add(NodeId::new(11).unwrap(), &[1.0, 0.0, 0.0, 0.0])?;

        assert_eq!(index.len(), 11);
        // Verify capacity expanded
        assert!(index.inner.read().capacity() > 10);

        Ok(())
    }

    #[test]
    fn test_occupied_path_inconsistency_race_recovery() -> Result<()> {
        // Setup: Ensure we skip the optimistic check
        TEST_SKIP_CAPACITY_CHECK.store(true, Ordering::SeqCst);
        struct ResetGuard;
        impl Drop for ResetGuard {
            fn drop(&mut self) {
                TEST_SKIP_CAPACITY_CHECK.store(false, Ordering::SeqCst);
            }
        }
        let _reset = ResetGuard;

        // Create index
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .initial_capacity(10)
            .build()?;

        // Fill to capacity
        for i in 0..10 {
            index.add(NodeId::new(i + 1).unwrap(), &[1.0, 0.0, 0.0, 0.0])?;
        }
        assert_eq!(index.len(), 10);

        // We want to trigger the Occupied path logic:
        // 1. Update an existing node (e.g., node 1).
        // 2. Use hook to make inner inconsistent (remove key 1 from inner).
        // 3. Ensure index is full (add dummy key to inner).

        let node_id = NodeId::new(1).unwrap();

        TEST_RACE_HOOK.with(|h| {
            h.set(Some(|idx, _id| {
                // Hook runs after acquiring map lock, before inner lock.
                // We need to modify inner state.
                let index = idx.inner.write();

                // Key for node 1 should be 0 (since it was first added)
                // Remove it from usearch
                let _ = index.remove(0); // Removing key 0 (node 1)

                // Now add a dummy key to fill the capacity back up
                // We need a key that doesn't conflict with map.
                // Map has keys 0..9.
                // We removed 0.
                // We add key 999.
                let _ = index.add(999, &[0.0, 1.0, 0.0, 0.0]);

                // Now size should be 10 again.
                // And key 0 is missing from inner.
            }))
        });

        // Trigger update
        // add(1) -> Map Occupied -> Hook runs -> Inner write lock
        // Inner contains(0)? False (removed in hook).
        // Enters else block.
        // Checks size >= capacity. 10 >= 10. True.
        // Expands.
        // Adds key 0.

        index.add(node_id, &[0.0, 1.0, 0.0, 0.0])?;

        // Cleanup
        TEST_RACE_HOOK.with(|h| h.set(None));

        assert_eq!(index.len(), 11); // 9 original + 1 dummy + 1 re-added node 1
        assert!(index.inner.read().capacity() > 10);

        Ok(())
    }
}

#[cfg(test)]
mod coverage_reentrancy_tests {
    use super::*;

    fn create_test_index() -> HnswIndex {
        HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap()
    }

    #[test]
    fn test_add_reentrancy_check() {
        let index = create_test_index();
        let node_id = NodeId::new(1).unwrap();
        let vec = vec![1.0, 0.0, 0.0, 0.0];

        // Simulate being inside a callback
        let _guard = FilterCallbackGuard::new();

        // add should fail
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

    #[test]
    fn test_remove_reentrancy_check() {
        let index = create_test_index();
        let node_id = NodeId::new(1).unwrap();

        // Simulate being inside a callback
        let _guard = FilterCallbackGuard::new();

        // remove should fail
        let result = index.remove(node_id);
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

    #[test]
    fn test_save_reentrancy_check() {
        let index = create_test_index();
        let path = Path::new("dummy.index");

        // Simulate being inside a callback
        let _guard = FilterCallbackGuard::new();

        // save should fail
        let result = index.save(path);
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::IndexError(msg))) => {
                assert!(
                    msg.contains("Cannot save index from within a search_with_filter callback")
                );
            }
            _ => panic!("Expected re-entrancy error"),
        }
    }

    #[test]
    fn test_search_reentrancy_check() {
        let index = create_test_index();
        let query = vec![1.0, 0.0, 0.0, 0.0];

        // Simulate being inside a callback
        let _guard = FilterCallbackGuard::new();

        // search should fail
        let result = index.search(&query, 10);
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::IndexError(msg))) => {
                assert!(
                    msg.contains("Cannot perform search from within a search_with_filter callback")
                );
            }
            _ => panic!("Expected re-entrancy error"),
        }
    }

    #[test]
    fn test_search_with_filter_reentrancy_check() {
        let index = create_test_index();
        let query = vec![1.0, 0.0, 0.0, 0.0];

        // Simulate being inside a callback
        let _guard = FilterCallbackGuard::new();

        // search_with_filter should fail
        let result = index.search_with_filter(&query, 10, |_| true);
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::IndexError(msg))) => {
                assert!(msg.contains(
                    "Cannot perform search_with_filter from within a search_with_filter callback"
                ));
            }
            _ => panic!("Expected re-entrancy error"),
        }
    }
}

#[cfg(test)]
mod coverage_misc_tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn test_index_stats_default() {
        // Cover #[derive(Default)] for IndexStats
        let stats = IndexStats::default();
        assert_eq!(
            stats
                .vectors_added
                .load(std::sync::atomic::Ordering::Relaxed),
            0
        );
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

#[cfg(test)]
mod coverage_additions {
    use super::*;
    use std::sync::atomic::Ordering;

    fn create_test_index() -> HnswIndex {
        HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap()
    }

    #[test]
    fn test_retry_usearch_logic() {
        let index = create_test_index();
        let mut attempts = 0;

        // We use a closure that simulates a transient error for the first 2 attempts
        // and then succeeds.
        // Or we can simulate total failure to verify it retries MAX_SEARCH_ATTEMPTS times.

        let result: crate::core::error::Result<()> = index.retry_usearch(
            || {
                attempts += 1;
                // Always fail with the specific retryable error message
                Err("No available threads to lock".to_string())
            },
            "test_context",
        );

        // Should fail after retries
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::IndexError(msg))) => {
                assert!(msg.contains("test_context"));
                assert!(msg.contains("No available threads to lock"));
            }
            _ => panic!("Expected IndexError"),
        }

        // MAX_SEARCH_ATTEMPTS is 4 (1 initial + 3 retries)
        assert_eq!(attempts, 4);

        // Verify stats were updated
        assert_eq!(index.stats.search_retries.load(Ordering::Relaxed), 3);
        assert_eq!(index.stats.search_retry_failures.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_retry_usearch_success_after_retry() {
        let index = create_test_index();
        let mut attempts = 0;

        let result: crate::core::error::Result<()> = index.retry_usearch(
            || {
                attempts += 1;
                if attempts < 3 {
                    Err("No available threads to lock".to_string())
                } else {
                    Ok(())
                }
            },
            "test_context",
        );

        assert!(result.is_ok());
        assert_eq!(attempts, 3);

        assert_eq!(index.stats.search_retries.load(Ordering::Relaxed), 2);
        // failures should be 0 (since it eventually succeeded)
        assert_eq!(index.stats.search_retry_failures.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_save_async_context() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("async_save.index");

        let index = create_test_index();
        index
            .add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0])
            .unwrap();

        // Call save inside a tokio runtime
        // This should trigger the block_in_place path
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();

        runtime.block_on(async {
            let result = index.save(&path);
            assert!(result.is_ok());
        });

        assert!(path.exists());
        assert!(path.with_extension("usearch.mappings").exists());
    }

    #[test]
    fn test_warden_coverage_search_happy_path() {
        // This test explicitly exercises the search function to ensure code coverage
        // hits the "Happy Path" lines inside the retry loop.
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap();

        let node_id = NodeId::new(1).unwrap();
        index.add(node_id, &[1.0, 0.0, 0.0, 0.0]).unwrap();
        // Call add again to hit Occupied path (line 900+ coverage)
        index.add(node_id, &[1.0, 0.0, 0.0, 0.0]).unwrap();

        // Call remove to hit Remove path
        index.remove(node_id).unwrap();

        // Re-add for search test
        index.add(node_id, &[1.0, 0.0, 0.0, 0.0]).unwrap();

        // Call search - should hit L1233, L1237, L1239
        let results = index.search(&[1.0, 0.0, 0.0, 0.0], 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, node_id);
    }

    #[test]
    fn test_warden_coverage_search_with_filter_happy_path() {
        // This test exercises search_with_filter to ensure coverage
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap();

        let node_id = NodeId::new(1).unwrap();
        index.add(node_id, &[1.0, 0.0, 0.0, 0.0]).unwrap();

        // Call search_with_filter
        let results = index
            .search_with_filter(&[1.0, 0.0, 0.0, 0.0], 10, |_| true)
            .unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_add_race_retry_value_change_coverage() {
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap();
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
        match result {
            Err(Error::Vector(VectorError::IndexError(msg))) => {
                assert!(msg.contains("Concurrent modification detected"));
                assert!(msg.contains("mapping changed"));
            }
            _ => panic!("Expected concurrent modification error"),
        }
    }

    #[test]
    fn test_add_race_retry_removal_coverage() {
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap();
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
        match result {
            Err(Error::Vector(VectorError::IndexError(msg))) => {
                assert!(msg.contains("Concurrent modification detected"));
                assert!(msg.contains("node removed"));
            }
            _ => panic!("Expected concurrent modification error"),
        }
    }

    #[test]
    fn test_add_race_vacant_coverage() {
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap();
        let node = NodeId::new(3).unwrap();

        TEST_RACE_HOOK.with(|h| {
            h.set(Some(|idx, node_id| {
                idx.id_mapping.insert(node_id, 999);
            }))
        });

        let result = index.add(node, &[0.5, 0.5, 0.5, 0.5]);

        TEST_RACE_HOOK.with(|h| h.set(None));

        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::IndexError(msg))) => {
                assert!(msg.contains("Concurrent add detected"));
                assert!(msg.contains("vector already exists"));
            }
            _ => panic!("Expected concurrent add error"),
        }

        assert_eq!(index.len(), 0);
    }

    #[test]
    fn test_load_mappings_bad_magic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_index.usearch");
        let mappings_path = path.with_extension("usearch.mappings");

        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap();
        index
            .add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0])
            .unwrap();
        index.save(&path).unwrap();

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
    }

    #[test]
    fn test_load_mappings_bad_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_index.usearch");
        let mappings_path = path.with_extension("usearch.mappings");

        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap();
        index
            .add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0])
            .unwrap();
        index.save(&path).unwrap();

        let mut data = std::fs::read(&mappings_path).unwrap();
        data[4] = 99; // Invalid version
        std::fs::write(&mappings_path, &data).unwrap();

        let result = HnswIndex::load(&path, HnswConfig::new(4, DistanceMetric::Cosine));
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::IndexError(msg))) => {
                assert!(msg.contains("Unsupported mapping file version"));
            }
            _ => panic!("Expected IndexError"),
        }
    }

    #[test]
    fn test_load_mappings_bad_crc() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_index.usearch");
        let mappings_path = path.with_extension("usearch.mappings");

        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap();
        index
            .add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0])
            .unwrap();
        index.save(&path).unwrap();

        let mut data = std::fs::read(&mappings_path).unwrap();
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
            _ => panic!("Expected IndexError"),
        }
    }

    #[test]
    fn test_load_mappings_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_index.usearch");
        let mappings_path = path.with_extension("usearch.mappings");

        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap();
        index
            .add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0])
            .unwrap();
        index.save(&path).unwrap();

        let data = std::fs::read(&mappings_path).unwrap();
        let truncated = &data[..10];
        std::fs::write(&mappings_path, truncated).unwrap();

        let result = HnswIndex::load(&path, HnswConfig::new(4, DistanceMetric::Cosine));
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::IndexError(msg))) => {
                assert!(msg.contains("too small"));
            }
            _ => panic!("Expected IndexError"),
        }
    }

    #[test]
    fn test_load_mappings_size_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_index.usearch");
        let mappings_path = path.with_extension("usearch.mappings");

        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap();
        index
            .add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0])
            .unwrap();
        index.save(&path).unwrap();

        let mut data = std::fs::read(&mappings_path).unwrap();
        let count_offset = 15;
        data[count_offset] = 2; // Make count larger

        // Recompute CRC to pass CRC check but fail size check
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
                assert!(msg.contains("size mismatch"));
            }
            _ => panic!("Expected IndexError"),
        }
    }

    #[test]
    fn test_load_mappings_overflow_header() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_index.usearch");
        let mappings_path = path.with_extension("usearch.mappings");

        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap();
        index
            .add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0])
            .unwrap();
        index.save(&path).unwrap();

        let mut data = std::fs::read(&mappings_path).unwrap();
        let count_offset = 15;
        let huge_count = u64::MAX;
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
                assert!(msg.contains("overflow") || msg.contains("exceeds maximum allowed"));
            }
            _ => panic!("Expected IndexError"),
        }
    }

    #[test]
    fn test_load_mappings_count_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_index.usearch");
        let mappings_path = path.with_extension("usearch.mappings");

        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap();
        index
            .add(NodeId::new(1).unwrap(), &[1.0, 0.0, 0.0, 0.0])
            .unwrap();
        index.save(&path).unwrap();

        let mut data = std::fs::read(&mappings_path).unwrap();
        let count_offset = 15;
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
    }

    #[test]
    fn test_save_mappings_large_streaming() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_streaming.usearch");

        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap();

        let count = 2000;
        for i in 1..=count {
            index
                .add(NodeId::new(i).unwrap(), &[1.0, 0.0, 0.0, 0.0])
                .unwrap();
        }

        index.save(&path).unwrap();

        let loaded = HnswIndex::load(&path, HnswConfig::new(4, DistanceMetric::Cosine)).unwrap();
        assert_eq!(loaded.len(), count as usize);
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
        let result = super::persistence::write_mappings_to_writer(
            &mut writer,
            mappings.iter().copied(),
            mappings.len(),
            &config,
        );
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::IndexError(msg))) => {
                assert!(msg.contains("Failed to flush mappings"));
            }
            _ => panic!("Expected IndexError"),
        }
    }

    #[test]
    fn test_save_mappings_file_create_error() {
        let dir = tempfile::tempdir().unwrap();
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .build()
            .unwrap();
        let index_path = dir.path().join("test.index");
        let mappings_path = index_path.with_extension("usearch.mappings");

        // Create directory blocking the mappings file path
        std::fs::create_dir(&mappings_path).unwrap();

        let result = index.save(&index_path);
        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::IndexError(msg))) => {
                assert!(msg.contains("Failed to create mappings file"));
            }
            _ => panic!("Expected IndexError"),
        }
    }
}

#[cfg(test)]
mod optimization_tests {
    use super::*;

    #[test]
    fn test_search_filter_optimization() -> Result<()> {
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .m(16)
            .ef_construction(100)
            .build()?;

        // Add 100 vectors
        for i in 1..=100 {
            let vec = vec![1.0, 0.0, 0.0, 0.0]; // All identical
            index.add(NodeId::new(i).unwrap(), &vec)?;
        }

        // Search with filter for even IDs, limit 5
        let results =
            index.search_with_filter(&[1.0, 0.0, 0.0, 0.0], 5, |id| id.as_u64() % 2 == 0)?;

        assert_eq!(results.len(), 5);
        for (id, _) in results {
            assert!(id.as_u64() % 2 == 0);
        }

        Ok(())
    }
}
