import re

# Remove unused imports from query/traits.rs
with open("src/query/traits.rs", "w") as f:
    f.write("// Traits module is now empty due to removal of GraphView.\n")

# Fix missing docs warning in src/index/vector/distributed.rs
with open("src/index/vector/distributed.rs", "r") as f:
    content = f.read()

content = content.replace("    pub fn new(node_id: u16, dimensions: usize, metric: DistanceMetric) -> Self {\n        Self {", "    /// Create a new MockVectorNodeClient\n    pub fn new(node_id: u16, dimensions: usize, metric: DistanceMetric) -> Self {\n        Self {")

with open("src/index/vector/distributed.rs", "w") as f:
    f.write(content)
