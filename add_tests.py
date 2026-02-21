import sys

with open("src/index/vector/hnsw.rs", "r") as f:
    lines = f.readlines()

new_tests = """
#[cfg(test)]
mod error_tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_collision_recovery_limit() {
        let index = HnswIndexBuilder::new(2, DistanceMetric::Cosine)
            .build()
            .unwrap();

        // Manually occupy keys 0..1100 in the internal index to force collisions
        // next_key starts at 0.
        {
            let mut inner = index.inner.write();
            // We need to use usearch API directly to bypass next_key updates
            // But we can't easily access usearch internals from here if they are not exposed.
            // HnswIndex wraps `usearch::Index`.
            // We can use `inner.add(key, vector)` directly?
            // HnswIndex::inner is Arc<RwLock<Index>>.
            // We can add vectors directly.

            for i in 0..1100 {
                inner.add(i, &[1.0, 0.0]).unwrap();
            }
        }

        // Now try to add a NEW node via public API.
        // It will try key=0 (collision), key=1 (collision)... key=1000 (collision).
        // Should eventually fail with "Too many key collisions"
        let result = index.add(NodeId::new(1).unwrap(), &[0.0, 1.0]);

        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::IndexError(msg))) => {
                assert!(msg.contains("Too many key collisions"), "Got error: {}", msg);
            }
            _ => panic!("Expected Too many key collisions error, got {:?}", result),
        }
    }

    #[test]
    fn test_save_index_rename_failure() {
        let dir = tempfile::tempdir().unwrap();
        let index = HnswIndexBuilder::new(2, DistanceMetric::Cosine)
            .build()
            .unwrap();

        // Setup paths
        let index_path = dir.path().join("test.index");

        // Create a directory at index_path to force index rename failure
        fs::create_dir(&index_path).unwrap();

        // mappings_path will be "test.usearch.mappings"
        // This path is free, so mappings rename should SUCCEED.

        let result = index.save(&index_path);

        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::IndexError(msg))) => {
                assert!(msg.contains("Failed to rename index file"), "Got error: {}", msg);
            }
            _ => panic!("Expected rename index file error, got {:?}", result),
        }

        // Verify mappings file exists (it was renamed successfully)
        let mappings_path = index_path.with_extension("usearch.mappings");
        assert!(mappings_path.exists());
    }
}
"""

lines.append(new_tests)

with open("src/index/vector/hnsw.rs", "w") as f:
    f.writelines(lines)
