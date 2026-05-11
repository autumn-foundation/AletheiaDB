"""Vector search quickstart.

Run with: python examples/vector_quick_start.py
"""
from aletheiadb import (
    AletheiaDB,
    DistanceMetric,
    HnswConfig,
    enable_vector_index,
    find_similar,
)


def main() -> None:
    db = AletheiaDB()
    enable_vector_index(db, "embedding", HnswConfig(4, DistanceMetric.COSINE))

    rust = db.create_node("Document", {
        "title": "Rust by Example",
        "embedding": [1.0, 0.0, 0.0, 0.0],
    })
    python = db.create_node("Document", {
        "title": "Python Programming",
        "embedding": [0.9, 0.1, 0.0, 0.0],
    })
    cooking = db.create_node("Document", {
        "title": "Cooking 101",
        "embedding": [0.0, 0.0, 1.0, 0.0],
    })

    print("Documents most similar to 'Rust by Example':")
    for node_id, score in find_similar(db, rust, k=3):
        title = db.get_node(node_id).properties["title"]
        print(f"  {title!r}: score={score:.4f}")


if __name__ == "__main__":
    main()
