//! Tapestry: Semantic Path Weaving Engine.
//!
//! "Weave a path through specific concepts."
//!
//! Tapestry builds on `SemanticNavigator` to find paths that don't just go from A to B,
//! but explicitly visit a sequence of "waypoints" along the way.
//! This allows forcing a semantic trajectory through specific conceptual neighborhoods.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::tapestry::TapestryEngine;
//! use aletheiadb::core::id::NodeId;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let tapestry = TapestryEngine::new(&db);
//!
//! # let start = NodeId::new(1).unwrap();
//! # let waypoint = NodeId::new(2).unwrap();
//! # let end = NodeId::new(3).unwrap();
//! // Weave a path from start to end, passing through 'waypoint'
//! let path = tapestry.weave(start, &[waypoint], end, "embedding")?;
//!
//! println!("Wove a path of {} nodes", path.len());
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;
use crate::experimental::semantic_navigator::SemanticNavigator;

/// The Tapestry Engine for waypoint-based semantic pathfinding.
pub struct TapestryEngine<'a> {
    db: &'a AletheiaDB,
}

impl<'a> TapestryEngine<'a> {
    /// Create a new TapestryEngine.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Weave a semantic path from `start_node` to `end_node`, visiting `waypoints` in order.
    ///
    /// # Arguments
    /// * `start_node` - The beginning of the path.
    /// * `waypoints` - A slice of intermediate nodes to visit in sequence.
    /// * `end_node` - The final destination.
    /// * `vector_property` - The name of the property containing vector embeddings.
    ///
    /// # Returns
    /// A single continuous `Vec<NodeId>` containing the full path, with duplicate
    /// waypoints (where segments join) merged.
    pub fn weave(
        &self,
        start_node: NodeId,
        waypoints: &[NodeId],
        end_node: NodeId,
        vector_property: &str,
    ) -> Result<Vec<NodeId>> {
        let navigator = SemanticNavigator::new(self.db);
        let mut full_path = Vec::new();

        // Build the sequence of points to visit: Start -> W1 -> W2 -> ... -> End
        let mut sequence = Vec::with_capacity(waypoints.len() + 2);
        sequence.push(start_node);
        sequence.extend_from_slice(waypoints);
        sequence.push(end_node);

        for window in sequence.windows(2) {
            let from = window[0];
            let to = window[1];

            let segment = navigator.find_path(from, to, vector_property)?;

            if full_path.is_empty() {
                full_path.extend(segment);
            } else {
                // Segment starts with 'from', which is already the last element of full_path
                // Skip the first element to avoid duplicates
                full_path.extend(segment.into_iter().skip(1));
            }
        }

        if full_path.is_empty() {
            // Edge case: start_node == end_node and no waypoints
            if start_node == end_node && waypoints.is_empty() {
                return Ok(vec![start_node]);
            } else {
                return Err(Error::other("Failed to weave path"));
            }
        }

        Ok(full_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AletheiaDBConfig;
    use crate::config::WalConfigBuilder;
    use crate::core::property::PropertyMapBuilder;
    use tempfile::tempdir;

    fn create_test_db() -> (AletheiaDB, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let config = AletheiaDBConfig::builder()
            .wal(
                WalConfigBuilder::new()
                    .wal_dir(dir.path().join("wal"))
                    .build(),
            )
            .build();
        let db = AletheiaDB::with_unified_config(config).unwrap();
        (db, dir)
    }

    #[test]
    fn test_tapestry_weave() {
        let (db, _dir) = create_test_db();

        // Topology:
        // A -> B -> C -> D -> E
        //
        // Waypoints: Weave from A to E through C.

        let props = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 0.0]) // Vectors don't matter much here, just need them for A*
            .build();

        let a = db.create_node("Node", props.clone()).unwrap();
        let b = db.create_node("Node", props.clone()).unwrap();
        let c = db.create_node("Node", props.clone()).unwrap();
        let d = db.create_node("Node", props.clone()).unwrap();
        let e = db.create_node("Node", props.clone()).unwrap();

        db.create_edge(a, b, "NEXT", PropertyMapBuilder::new().build())
            .unwrap();
        db.create_edge(b, c, "NEXT", PropertyMapBuilder::new().build())
            .unwrap();
        db.create_edge(c, d, "NEXT", PropertyMapBuilder::new().build())
            .unwrap();
        db.create_edge(d, e, "NEXT", PropertyMapBuilder::new().build())
            .unwrap();

        let tapestry = TapestryEngine::new(&db);
        let path = tapestry.weave(a, &[c], e, "vec").unwrap();

        assert_eq!(path, vec![a, b, c, d, e]);
    }
}
