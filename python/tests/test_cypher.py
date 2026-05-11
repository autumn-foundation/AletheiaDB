from aletheiadb import AletheiaDB


def test_execute_cypher_basic():
    db = AletheiaDB()
    db.create_node("Person", {"name": "Alice"})
    db.create_node("Person", {"name": "Bob"})
    rows = db.execute_cypher("MATCH (n:Person) RETURN n")
    assert len(rows) == 2
    for row in rows:
        assert row["kind"] in {"node", "node_id"}


def test_execute_cypher_with_params():
    db = AletheiaDB()
    db.create_node("Person", {"name": "Alice"})
    db.create_node("Person", {"name": "Bob"})
    rows = db.execute_cypher(
        "MATCH (n:Person {name: $name}) RETURN n",
        params={"name": "Alice"},
    )
    assert len(rows) == 1
