"""AletheiaDB Python SDK.

A high-performance bi-temporal graph database with native Python bindings.

Quickstart:

    from aletheiadb import AletheiaDB

    db = AletheiaDB()
    alice = db.create_node("Person", {"name": "Alice", "age": 30})
    bob = db.create_node("Person", {"name": "Bob"})
    db.create_edge(alice, bob, "KNOWS", {})

    node = db.get_node(alice)
    print(node.label, node.properties)
"""

from __future__ import annotations

from . import _native
from ._native import (
    AletheiaDB,
    AletheiaDBError,
    ConfigError,
    DistanceMetric,
    Edge,
    EdgeNotFound,
    HnswConfig,
    InvalidId,
    IoError,
    Node,
    NodeNotFound,
    QueryError,
    TemporalError,
    enable_vector_index,
    execute_aql,
    find_similar,
    find_similar_by_vector,
)

__version__ = _native.__version__

__all__ = [
    "AletheiaDB",
    "AletheiaDBError",
    "ConfigError",
    "DistanceMetric",
    "Edge",
    "EdgeNotFound",
    "HnswConfig",
    "InvalidId",
    "IoError",
    "Node",
    "NodeNotFound",
    "QueryError",
    "TemporalError",
    "__version__",
    "enable_vector_index",
    "execute_aql",
    "find_similar",
    "find_similar_by_vector",
]
