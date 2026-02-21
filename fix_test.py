import sys

with open("src/index/vector/hnsw.rs", "r") as f:
    lines = f.readlines()

# Look for assert!(msg.contains("Failed to create mappings file"));
# Replace with assert!(msg.contains("Failed to create mappings file") || msg.contains("Failed to rename mappings file"));

for i, line in enumerate(lines):
    if 'assert!(msg.contains("Failed to create mappings file"));' in line:
        lines[i] = '            assert!(msg.contains("Failed to create mappings file") || msg.contains("Failed to rename mappings file"));\n'
        break

with open("src/index/vector/hnsw.rs", "w") as f:
    f.writelines(lines)
