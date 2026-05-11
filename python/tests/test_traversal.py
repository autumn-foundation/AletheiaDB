from aletheiadb import AletheiaDB


def test_adjacency():
    db = AletheiaDB()
    a = db.create_node("Person", {})
    b = db.create_node("Person", {})
    c = db.create_node("Person", {})
    e1 = db.create_edge(a, b, "KNOWS", {})
    e2 = db.create_edge(a, c, "WORKS_WITH", {})
    db.create_edge(c, a, "FOLLOWS", {})

    out_all = db.outgoing_edges(a)
    assert sorted(out_all) == sorted([e1, e2])

    out_knows = db.outgoing_edges(a, label="KNOWS")
    assert out_knows == [e1]

    incoming = db.incoming_edges(a)
    assert len(incoming) == 1


def test_degrees():
    db = AletheiaDB()
    a = db.create_node("X", {})
    b = db.create_node("X", {})
    c = db.create_node("X", {})
    db.create_edge(a, b, "R", {})
    db.create_edge(a, c, "R", {})
    db.create_edge(b, a, "R", {})
    assert db.out_degree(a) == 2
    assert db.in_degree(a) == 1


def test_traverse_bfs():
    db = AletheiaDB()
    a = db.create_node("X", {})
    b = db.create_node("X", {})
    c = db.create_node("X", {})
    db.create_edge(a, b, "R", {})
    db.create_edge(b, c, "R", {})
    results = db.traverse(a, label="R", max_depth=2, direction="out")
    node_ids = [r["node_id"] for r in results]
    assert a in node_ids and b in node_ids and c in node_ids
    depths = {r["node_id"]: r["depth"] for r in results}
    assert depths[a] == 0
    assert depths[b] == 1
    assert depths[c] == 2


def test_traverse_depth_limit():
    db = AletheiaDB()
    a = db.create_node("X", {})
    b = db.create_node("X", {})
    c = db.create_node("X", {})
    db.create_edge(a, b, "R", {})
    db.create_edge(b, c, "R", {})
    results = db.traverse(a, label="R", max_depth=1)
    node_ids = [r["node_id"] for r in results]
    assert c not in node_ids
