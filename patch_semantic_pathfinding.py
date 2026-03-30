with open("src/query/semantic_pathfinding.rs", "r") as f:
    content = f.read()

content = content.replace("use crate::query::traits::GraphView;\n", "")
content = content.replace("pub struct SemanticPathfinder<'a, G: GraphView + ?Sized> {\n    db: &'a G,", "pub struct SemanticPathfinder<'a> {\n    db: &'a crate::db::AletheiaDB,")
content = content.replace("impl<'a, G: GraphView + ?Sized> SemanticPathfinder<'a, G> {", "impl<'a> SemanticPathfinder<'a> {")

with open("src/query/semantic_pathfinding.rs", "w") as f:
    f.write(content)
