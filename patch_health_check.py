import re

with open("src/index/vector/distributed.rs", "r") as f:
    content = f.read()

# MockVectorNodeClient's health_check is unused now that the trait is gone. Wait, the execute method doesn't use it?
# Let's search for health_check in the file.
content = content.replace("fn health_check(&self) -> Result<()> {\n        self.check_fail()?;\n\n        if self.is_healthy() {\n            Ok(())\n        } else {\n            Err(Error::Vector(VectorError::IndexError(format!(\n                \"Node {} is unavailable\",\n                self.node_id\n            ))))\n        }\n    }\n", "")

with open("src/index/vector/distributed.rs", "w") as f:
    f.write(content)
