import re

with open("src/index/vector/distributed.rs", "r") as f:
    content = f.read()

content = content.replace("fn health_check(&self) -> Result<()> {\n        self.check_fail()?;\n\n        if self.is_healthy() {\n            Ok(())\n        } else {\n            Err(Error::Vector(VectorError::IndexError(format!(\n                \"Node {} is unavailable\",\n                self.node_id\n            ))))\n        }\n    }\n", "")

# Add is_empty back to MockVectorNodeClient since it was a default trait method
add_is_empty = """
    pub fn is_empty(&self) -> Result<bool> {
        self.len().map(|len| len == 0)
    }
"""

content = content.replace("pub fn new(node_id: u16, dimensions: usize, metric: DistanceMetric) -> Self {", add_is_empty + "\n    pub fn new(node_id: u16, dimensions: usize, metric: DistanceMetric) -> Self {")


with open("src/index/vector/distributed.rs", "w") as f:
    f.write(content)
