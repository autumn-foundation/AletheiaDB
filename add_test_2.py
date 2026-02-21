import sys

with open("src/index/vector/hnsw.rs", "r") as f:
    lines = f.readlines()

new_test = """
    #[test]
    fn test_save_internal_temp_create_error() {
        let index = HnswIndexBuilder::new(2, DistanceMetric::Cosine)
            .build()
            .unwrap();

        // Use a path in a non-existent directory
        // On Linux, /nonexistent/ usually doesn't exist.
        // Or use a relative path with missing parent.
        let path = Path::new("nonexistent_dir/test.index");

        let result = index.save(path);

        assert!(result.is_err());
        match result {
            Err(Error::Vector(VectorError::IndexError(msg))) => {
                assert!(msg.contains("Failed to save index to temp file"), "Got error: {}", msg);
            }
            _ => panic!("Expected save index temp file error, got {:?}", result),
        }
    }
"""

# Insert before the last closing brace (end of mod error_tests)
# Find the last closing brace
for i in range(len(lines)-1, -1, -1):
    if lines[i].strip() == "}":
        # Check if it's the module closing brace
        # We assume the last line is the module closing brace
        lines.insert(i, new_test)
        break

with open("src/index/vector/hnsw.rs", "w") as f:
    f.writelines(lines)
