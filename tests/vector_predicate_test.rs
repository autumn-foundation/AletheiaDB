use aletheiadb::{AletheiaDB, PropertyMapBuilder};
use aletheiadb::index::vector::{HnswConfig, DistanceMetric};
use aletheiadb::utils::Result;

#[test]
fn test_find_similar_with_predicate() -> Result<()> {
    // 1. Setup DB and vector index
    let db = AletheiaDB::new()?;
    let config = HnswConfig::new(2, DistanceMetric::Cosine);
    db.enable_vector_index("embedding", config)?;

    // 2. Insert nodes with vectors and metadata
    // Node 1: [1.0, 0.0], category="A"
    let props1 = PropertyMapBuilder::new()
        .insert("category", "A")
        .insert_vector("embedding", &[1.0, 0.0])
        .build();
    let node1 = db.create_node("Item", props1)?;

    // Node 2: [0.9, 0.1], category="B" (Similar to 1, but diff category)
    let props2 = PropertyMapBuilder::new()
        .insert("category", "B")
        .insert_vector("embedding", &[0.9, 0.1])
        .build();
    let node2 = db.create_node("Item", props2)?;

    // Node 3: [0.0, 1.0], category="A" (Dissimilar to 1, same category)
    let props3 = PropertyMapBuilder::new()
        .insert("category", "A")
        .insert_vector("embedding", &[0.0, 1.0])
        .build();
    let node3 = db.create_node("Item", props3)?;

    // 3. Search with predicate: Filter for category="A"
    // Query vector matches Node 1 perfectly. Node 2 is close but wrong category. Node 3 is far.
    // We expect Node 1 to be returned. Node 2 should be filtered out.

    let query = vec![1.0, 0.0];
    let results = db.find_similar_with_predicate(
        "embedding",
        &query,
        10,
        |node_id| {
            if let Ok(node) = db.get_node(*node_id)
                && let Some(val) = node.properties.get("category")
            {
                return val.as_str() == Some("A");
            }
            false
        }
    )?;

    // 4. Verify results
    // Should contain Node 1 (best match, category A)
    // Should NOT contain Node 2 (good match, but category B)
    // Might contain Node 3 (poor match, category A), depending on recall/k

    let ids: Vec<_> = results.iter().map(|(id, _)| *id).collect();
    assert!(ids.contains(&node1), "Node 1 should be found");
    assert!(!ids.contains(&node2), "Node 2 should be filtered out by predicate");

    // Also verify Node 3 is found if k is large enough (it satisfies predicate)
    assert!(ids.contains(&node3), "Node 3 should be found (satisfies predicate)");

    Ok(())
}
