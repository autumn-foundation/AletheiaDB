//! Sentinel targeted tests for Semantic Pathfinding mutants.
//!
//! These tests are specifically designed to catch regressions in semantic cost calculation
//! and pathfinding logic identified by mutation testing.

use aletheiadb::config::{AletheiaDBConfig, WalConfigBuilder};
use aletheiadb::core::property::PropertyMapBuilder;
use aletheiadb::index::vector::{DistanceMetric, HnswConfig};
use aletheiadb::query::semantic_pathfinding::SemanticPathfinder;
use aletheiadb::{AletheiaDB, WriteOps};
use tempfile::TempDir;

fn create_test_db() -> (AletheiaDB, TempDir) {
    let temp_dir = TempDir::new().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let config = AletheiaDBConfig::builder()
        .wal(WalConfigBuilder::new().wal_dir(wal_dir).build())
        .build();

    let db = AletheiaDB::with_unified_config(config).unwrap();

    // Enable vector index
    db.vector_index("embedding")
        .hnsw(HnswConfig::new(3, DistanceMetric::Cosine))
        .enable()
        .unwrap();
    (db, temp_dir)
}

#[test]
fn test_semantic_pathfinding_penalizes_opposite() {
    // 🎯 Target: Kills `replace - with /` in `1.0 - sim` (line 292)
    // 💣 Risk: If cost = 1.0 / sim, negative similarity (opposite vectors) becomes negative cost,
    // which effectively rewards dissimilarity instead of penalizing it.

    let (db, _temp_dir) = create_test_db();

    // Start -> End
    let start = db
        .create_node("Start", PropertyMapBuilder::new().build())
        .unwrap();
    let end = db
        .create_node("End", PropertyMapBuilder::new().build())
        .unwrap();

    // Query vector: [1.0, 0.0, 0.0]
    let query = vec![1.0, 0.0, 0.0];

    // Path 1: "Neutral" node (orthogonal, sim = 0.0)
    // Cost should be 1.0 - 0.0 = 1.0.
    // Structural cost adds 0.1 -> Total ~1.1 per hop.
    let neutral = db
        .create_node(
            "Neutral",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &[0.0, 1.0, 0.0])
                .build(),
        )
        .unwrap();

    // Path 2: "Opposite" node (opposite, sim = -0.9ish)
    // Cost should be 1.0 - (-0.9) = 1.9 (Very high).
    // If mutant `1.0 / sim`: 1.0 / -0.9 = -1.1 (Very low/negative).
    let opposite = db
        .create_node(
            "Opposite",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &[-0.9, 0.0, 0.0])
                .build(),
        )
        .unwrap();

    // Connect paths
    // Start -> Neutral -> End
    db.create_edge(start, neutral, "NEXT", PropertyMapBuilder::new().build())
        .unwrap();
    db.create_edge(neutral, end, "NEXT", PropertyMapBuilder::new().build())
        .unwrap();

    // Start -> Opposite -> End
    db.create_edge(start, opposite, "NEXT", PropertyMapBuilder::new().build())
        .unwrap();
    db.create_edge(opposite, end, "NEXT", PropertyMapBuilder::new().build())
        .unwrap();

    let pathfinder = SemanticPathfinder::new(&db, "embedding");
    let path = pathfinder
        .find_path(start, end, &query, 10, false)
        .unwrap()
        .expect("Path should be found");

    // Assert we chose the Neutral path (sim=0) over Opposite path (sim=-0.9)
    // because 0.0 > -0.9 in similarity, so cost(0.0) < cost(-0.9).
    assert_eq!(path.len(), 3);
    assert_eq!(
        path[1], neutral,
        "Should prefer neutral node (cost 1.0) over opposite node (cost 1.9)"
    );
}

#[test]
fn test_semantic_pathfinding_overcomes_structural_cost() {
    // 🎯 Target: Kills constant cost mutants `Ok(1.0)`, `Ok(0.0)`
    // 💣 Risk: If semantic cost is ignored, pathfinding degrades to BFS (shortest path).
    // We construct a case where the semantically "good" path is longer (more hops)
    // but semantically cheaper than the "bad" path.

    let (db, _temp_dir) = create_test_db();

    let start = db
        .create_node("Start", PropertyMapBuilder::new().build())
        .unwrap();
    let end = db
        .create_node("End", PropertyMapBuilder::new().build())
        .unwrap();
    let query = vec![1.0, 0.0, 0.0];

    // Path 1: "Good" but long (3 hops)
    // Nodes match query perfectly (sim = 1.0, cost = 0.0).
    // Total cost calculation:
    // Start -> Good1 -> Good2 -> End.
    // Cost(Good1) = 0.0 (perfect match) + 0.1 (structural).
    // Cost(Good2) = 0.0 (perfect match) + 0.1 (structural).
    // Cost(End)   = 0.0 (perfect match) + 0.1 (structural).
    // Total Cost = 0.3.

    // Ensure End node has an embedding so cost calculation is consistent
    db.write(|tx| {
        tx.update_node(
            end,
            PropertyMapBuilder::new()
                .insert_vector("embedding", &[1.0, 0.0, 0.0])
                .build(),
        )
    })
    .unwrap();

    // Construct the Good Path: Start -> Good1 -> Good2 -> End
    let good1 = db
        .create_node(
            "Good1",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &[1.0, 0.0, 0.0])
                .build(),
        )
        .unwrap();
    let good2 = db
        .create_node(
            "Good2",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &[1.0, 0.0, 0.0])
                .build(),
        )
        .unwrap();

    db.create_edge(start, good1, "NEXT", PropertyMapBuilder::new().build())
        .unwrap();
    db.create_edge(good1, good2, "NEXT", PropertyMapBuilder::new().build())
        .unwrap();
    db.create_edge(good2, end, "NEXT", PropertyMapBuilder::new().build())
        .unwrap();

    // Path 2: "Bad" but short (2 hops)
    // Start -> Bad -> End
    // Bad node is opposite (sim = -1.0, cost = 2.0).
    // Cost(Bad) = 2.0 (opposite) + 0.1 (structural) = 2.1.
    // Cost(End) = 0.0 (perfect match) + 0.1 (structural) = 0.1.
    // Total Cost = 2.2.

    let bad = db
        .create_node(
            "Bad",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &[-1.0, 0.0, 0.0])
                .build(),
        )
        .unwrap();

    db.create_edge(start, bad, "NEXT", PropertyMapBuilder::new().build())
        .unwrap();
    db.create_edge(bad, end, "NEXT", PropertyMapBuilder::new().build())
        .unwrap();

    // Analysis:
    // Correct Logic: Cost(Good Path) 0.3 < Cost(Bad Path) 2.2. Should choose Good Path.

    // Mutant Ok(1.0) (Constant cost):
    // Good Path Cost: (1.0 + 0.1) * 3 nodes = 3.3.
    // Bad Path Cost:  (1.0 + 0.1) * 2 nodes = 2.2.
    // Mutant Logic: 3.3 > 2.2. Should choose Bad Path (Shortest path/BFS).

    let pathfinder = SemanticPathfinder::new(&db, "embedding");
    let path = pathfinder
        .find_path(start, end, &query, 10, false)
        .unwrap()
        .expect("Path should be found");

    // Should choose the longer Good path
    assert_eq!(
        path.len(),
        4,
        "Should choose longer path (len 4) because it is semantically cheaper"
    );
    assert_eq!(path[1], good1);
    assert_eq!(path[2], good2);
}
