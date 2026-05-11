"""Bi-temporal history walkthrough.

Run with: python examples/time_travel.py
"""
from aletheiadb import AletheiaDB


def main() -> None:
    db = AletheiaDB()

    alice = db.create_node("Person", {"name": "Alice", "age": 30})
    db.update_node(alice, {"name": "Alice", "age": 31})
    db.update_node(alice, {"name": "Alice", "age": 32, "city": "Berlin"})

    print(f"Current Alice: {db.get_node(alice).properties}")
    print()
    print("Full history:")
    for version in db.node_history(alice):
        print(
            f"  v{version['version_number']:>2}  "
            f"valid_from={version['valid_from_micros']}  "
            f"props={version['properties']}"
        )


if __name__ == "__main__":
    main()
