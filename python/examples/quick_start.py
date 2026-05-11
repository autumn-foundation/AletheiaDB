"""Mirror of examples/quick_start.rs.

Run with: python examples/quick_start.py
"""
from aletheiadb import AletheiaDB


def main() -> None:
    db = AletheiaDB()

    alice = db.create_node("Person", {"name": "Alice", "age": 30})
    bob = db.create_node("Person", {"name": "Bob"})
    db.create_edge(alice, bob, "KNOWS", {})

    node = db.get_node(alice)
    print(f"Created Alice: {node}")
    print(f"Label: {node.label}")
    print(f"Properties: {node.properties}")

    historical = db.get_node_at_time(alice)
    print(f"Alice's age at query time: {historical.properties.get('age')}")


if __name__ == "__main__":
    main()
