
#[cfg(test)]
mod tests {
    use aletheiadb::{AletheiaDB, Error, NodeId};
    use aletheiadb::api::transaction::WriteOps;
    use aletheiadb::core::property::PropertyMap;

    #[test]
    fn test_create_edge_on_deleted_node_in_same_tx() {
        let db = AletheiaDB::new().unwrap();

        // 1. Create two nodes
        let (node1, node2) = db.write(|tx| -> Result<(NodeId, NodeId), Error> {
            let n1 = tx.create_node("Person", PropertyMap::new())?;
            let n2 = tx.create_node("Person", PropertyMap::new())?;
            Ok((n1, n2))
        }).unwrap();

        // 2. In a new transaction: delete node1, then try to create an edge from node1 to node2
        let result: Result<(), Error> = db.write(|tx| {
            // Delete the source node
            tx.delete_node(node1)?;

            // Try to create an edge from the deleted node
            // This SHOULD fail validation
            tx.create_edge(node1, node2, "GHOST_EDGE", PropertyMap::new())?;

            Ok(())
        });

        // 3. Verify the result
        assert!(result.is_err(), "Should not be able to create edge on deleted node");

        // 4. If it succeeded (bug exists), verify state
        if result.is_ok() {
            println!("Bug confirmed: Transaction committed successfully");

            // Verify edge count via statistics
            let stats = db.statistics();
            println!("Edge count: {}", stats.edge_count());

            // If edge_count > 0, we have a dangling edge
            assert_eq!(stats.edge_count(), 0, "Should have 0 edges");
        }
    }
}
