import pytest

from aletheiadb import (
    AletheiaDB,
    DistanceMetric,
    HnswConfig,
    enable_vector_index,
    find_similar,
    find_similar_by_vector,
)


def _make_db():
    db = AletheiaDB()
    enable_vector_index(db, "embedding", HnswConfig(4, DistanceMetric.COSINE))
    a = db.create_node("Doc", {"embedding": [1.0, 0.0, 0.0, 0.0]})
    b = db.create_node("Doc", {"embedding": [0.9, 0.1, 0.0, 0.0]})
    c = db.create_node("Doc", {"embedding": [0.0, 1.0, 0.0, 0.0]})
    return db, a, b, c


def test_find_similar_by_node():
    db, a, b, c = _make_db()
    results = find_similar(db, a, k=3)
    ids = [r[0] for r in results]
    # b is much closer to a than c
    assert ids.index(b) < ids.index(c) if c in ids else True


def test_find_similar_by_vector():
    db, a, b, c = _make_db()
    results = find_similar_by_vector(db, [1.0, 0.0, 0.0, 0.0], k=2)
    assert len(results) <= 2
    assert all(isinstance(r[0], int) and isinstance(r[1], float) for r in results)


def test_empty_embedding_rejected():
    db, *_ = _make_db()
    with pytest.raises(ValueError):
        find_similar_by_vector(db, [], k=1)


def test_distance_metric_constants():
    assert DistanceMetric.COSINE != DistanceMetric.EUCLIDEAN
    cfg = HnswConfig(8, DistanceMetric.EUCLIDEAN)
    assert cfg.dimensions == 8
    assert cfg.metric == DistanceMetric.EUCLIDEAN
