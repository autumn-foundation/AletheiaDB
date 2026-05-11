import pytest

from aletheiadb import AletheiaDB, Edge, Node, NodeNotFound


def test_create_and_get_node():
    db = AletheiaDB()
    nid = db.create_node("Person", {"name": "Alice", "age": 30})
    assert isinstance(nid, int)
    node = db.get_node(nid)
    assert isinstance(node, Node)
    assert node.label == "Person"
    assert node.properties == {"name": "Alice", "age": 30}


def test_node_without_properties():
    db = AletheiaDB()
    nid = db.create_node("Empty")
    node = db.get_node(nid)
    assert node.properties == {}


def test_create_edge():
    db = AletheiaDB()
    a = db.create_node("Person", {"name": "Alice"})
    b = db.create_node("Person", {"name": "Bob"})
    eid = db.create_edge(a, b, "KNOWS", {"since": 2024})
    edge = db.get_edge(eid)
    assert isinstance(edge, Edge)
    assert edge.label == "KNOWS"
    assert edge.source_id == a
    assert edge.target_id == b
    assert edge.properties == {"since": 2024}


def test_update_node():
    db = AletheiaDB()
    nid = db.create_node("Person", {"name": "Alice", "age": 30})
    db.update_node(nid, {"name": "Alice", "age": 31})
    assert db.get_node(nid).properties == {"name": "Alice", "age": 31}


def test_delete_node():
    db = AletheiaDB()
    nid = db.create_node("Person", {"name": "Alice"})
    db.delete_node(nid)
    with pytest.raises(NodeNotFound):
        db.get_node(nid)


def test_delete_node_cascade():
    db = AletheiaDB()
    a = db.create_node("Person", {})
    b = db.create_node("Person", {})
    eid = db.create_edge(a, b, "KNOWS", {})
    db.delete_node_cascade(a)
    with pytest.raises(NodeNotFound):
        db.get_node(a)


def test_counts_and_listing():
    db = AletheiaDB()
    for i in range(5):
        db.create_node("Person", {"i": i})
    assert db.count_nodes() == 5
    assert len(db.list_nodes()) == 5
    assert len(db.nodes_by_label("Person")) == 5
    assert db.nodes_by_label("Missing") == []


def test_property_types():
    db = AletheiaDB()
    nid = db.create_node("X", {
        "string": "hello",
        "int": 42,
        "float": 3.14,
        "bool": True,
        "bytes": b"\x00\x01\x02",
        "list_mixed": ["a", 1, True],
        "vector": [0.1, 0.2, 0.3, 0.4],
        "none": None,
    })
    props = db.get_node(nid).properties
    assert props["string"] == "hello"
    assert props["int"] == 42
    assert props["float"] == 3.14
    assert props["bool"] is True
    assert props["bytes"] == b"\x00\x01\x02"
    assert props["list_mixed"] == ["a", 1, True]
    assert props["vector"] == pytest.approx([0.1, 0.2, 0.3, 0.4], abs=1e-6)
    assert props["none"] is None


def test_repr():
    db = AletheiaDB()
    db.create_node("X", {})
    assert "AletheiaDB" in repr(db)
    assert "nodes=1" in repr(db)
