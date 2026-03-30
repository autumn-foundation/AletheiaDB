with open("src/db/tests.rs", "r") as f:
    content = f.read()

content = content.replace("use crate::query::semantic_pathfinding::*;", "")

with open("src/db/tests.rs", "w") as f:
    f.write(content)
