from aletheiadb import AletheiaDB


def test_node_history_grows_with_updates():
    db = AletheiaDB()
    nid = db.create_node("X", {"v": 1})
    db.update_node(nid, {"v": 2})
    db.update_node(nid, {"v": 3})
    history = db.node_history(nid)
    versions = [v["properties"]["v"] for v in history]
    assert versions == [1, 2, 3]


def test_history_record_shape():
    db = AletheiaDB()
    nid = db.create_node("X", {"v": 1})
    history = db.node_history(nid)
    record = history[0]
    assert {
        "version_id",
        "version_number",
        "label",
        "properties",
        "valid_from_micros",
        "valid_to_micros",
        "tx_from_micros",
        "tx_to_micros",
    } <= set(record.keys())


def test_get_node_at_time_now():
    db = AletheiaDB()
    nid = db.create_node("X", {"v": 1})
    # No times specified -> now
    n = db.get_node_at_time(nid)
    assert n.properties == {"v": 1}
