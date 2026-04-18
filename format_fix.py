import os

file_path = "src/index/vector/distributed.rs"

with open(file_path, "r") as f:
    content = f.read()

# We need to move the `test_mock_client_euclidean_search` inside the `mod tests` block.
# Let's find the closing brace of the file.

# The last part of the file before my addition was:
#         assert!(!index.needs_rebalancing(RECOMMENDED_IMBALANCE_THRESHOLD));
#
#         Ok(())
#     }
# }

# So we can remove what we appended and insert it properly.
test_to_remove = """
    #[test]
    fn test_mock_client_euclidean_search() -> Result<()> {
        let client = MockVectorNodeClient::new(0, 4, DistanceMetric::Euclidean);

        let node = NodeId::new(1).unwrap();
        client.add(node, &[1.0, 0.0, 0.0, 0.0])?;

        let results = client.search(&[1.0, 0.0, 0.0, 0.0], 10)?;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, node);
        assert_eq!(results[0].1, 0.0);

        let results_far = client.search(&[-1.0, 0.0, 0.0, 0.0], 10)?;
        assert_eq!(results_far.len(), 1);
        assert_eq!(results_far[0].0, node);
        assert!(results_far[0].1 < 0.0);

        Ok(())
    }
"""

if test_to_remove in content:
    content = content.replace(test_to_remove, "")
else:
    # Try with stripping spaces
    if test_to_remove.strip() in content:
        content = content.replace(test_to_remove.strip(), "")
    else:
        print("Test not found exactly")

        lines = content.splitlines()
        while len(lines) > 0 and 'fn test_mock_client_euclidean_search' in '\n'.join(lines[-25:]):
            lines.pop()
        content = '\n'.join(lines) + '\n'

# Now append it right before the last closing brace
# Find the last closing brace

last_brace_idx = content.rfind("}")

new_content = content[:last_brace_idx] + """    #[test]
    fn test_mock_client_euclidean_search() -> Result<()> {
        let client = MockVectorNodeClient::new(0, 4, DistanceMetric::Euclidean);

        let node = NodeId::new(1).unwrap();
        client.add(node, &[1.0, 0.0, 0.0, 0.0])?;

        let results = client.search(&[1.0, 0.0, 0.0, 0.0], 10)?;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, node);
        assert_eq!(results[0].1, 0.0);

        let results_far = client.search(&[-1.0, 0.0, 0.0, 0.0], 10)?;
        assert_eq!(results_far.len(), 1);
        assert_eq!(results_far[0].0, node);
        assert!(results_far[0].1 < 0.0);

        Ok(())
    }
}
"""

with open(file_path, "w") as f:
    f.write(new_content)

print("Moved test inside `mod tests`")
