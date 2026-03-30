with open("src/query/hybrid.rs", "r") as f:
    content = f.read()

content = content.replace("use crate::query::traits::GraphView;\n", "")
content = content.replace("pub fn traverse_and_rank<G: GraphView + ?Sized>(\n    db: &G,", "pub fn traverse_and_rank(\n    db: &crate::db::AletheiaDB,")
content = content.replace("pub fn find_similar_as_of<G: GraphView + ?Sized>(\n    db: &G,", "pub fn find_similar_as_of(\n    db: &crate::db::AletheiaDB,")

with open("src/query/hybrid.rs", "w") as f:
    f.write(content)
