import re

with open("src/index/vector/distributed.rs", "r") as f:
    content = f.read()

content = content.replace("DistributedVectorIndex::<MockVectorNodeClient>::", "DistributedVectorIndex::")
content = content.replace("let result = connection.execute(|c| c.health_check());", "let result = connection.execute(|c| c.len());")

with open("src/index/vector/distributed.rs", "w") as f:
    f.write(content)

with open("src/query/semantic_pathfinding.rs", "r") as f:
    content = f.read()

content = content.replace("pub fn new(db: &'a G, vector_property: &str) -> Self {", "pub fn new(db: &'a crate::db::AletheiaDB, vector_property: &str) -> Self {")

with open("src/query/semantic_pathfinding.rs", "w") as f:
    f.write(content)
