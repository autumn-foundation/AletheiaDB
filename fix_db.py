with open("src/db/tests.rs", "r") as f:
    content = f.read()

content = content.replace("use crate::query::hybrid::*;", "use crate::query::hybrid::*;\n    use crate::core::id::NodeId;\n    use crate::api::transaction::WriteTransaction;")
content = content.replace("use crate::query::semantic_pathfinding::*;\n    use crate::query::SemanticPathfinder;", "use crate::query::semantic_pathfinding::*;\n    use crate::query::SemanticPathfinder;\n    use crate::api::transaction::WriteTransaction;")

with open("src/db/tests.rs", "w") as f:
    f.write(content)
