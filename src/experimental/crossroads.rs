//! Crossroads: Semantic Divergence Router.
//!
//! "When you come to a fork in the road, take the most interesting ones."
//!
//! While standard vector searches find the *most similar* nodes,
//! the Semantic Divergence Router (Crossroads) explores a node's outgoing
//! neighbors and selects a set of `k` paths that are **maximally diverse**
//! from each other.
//!
//! # Concepts
//! - **Divergent Paths**: Choosing the next steps in a traversal such that
//!   they explore orthogonal semantic domains, rather than clustered similar concepts.
//! - **Greedy Max-Min Distance**: An algorithm that iteravely selects the neighbor
//!   whose minimum distance to the already selected neighbors is maximized.
//!
//! # Use Cases
//! - **Diverse Recommendations**: Giving users a variety of options (e.g. "If you like X, you might also like... (Action), (Comedy), (Sci-Fi)")
//! - **Exploratory Navigation**: Breaking out of filter bubbles by intentionally providing orthogonal pathways.
//!
//! # Example
//! ```rust,no_run
//! // Requires features = ["nova"]
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::crossroads::Crossroads;
//! use aletheiadb::core::id::NodeId;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let crossroads = Crossroads::new(&db);
//!
//! # let start_node = NodeId::new(1)?;
//! // Find 3 maximally diverse outgoing neighbors based on the "embedding" property
//! let diverse_paths = crossroads.find_divergent_paths(start_node, "embedding", 3)?;
//! # Ok(())
//! # }
//! ```

#![allow(clippy::collapsible_if)]

use crate::AletheiaDB;
use crate::api::transaction::ReadOps;
use crate::core::error::{Error, Result, VectorError};
use crate::core::id::NodeId;
use crate::core::vector::ops::cosine_similarity;

/// The Semantic Divergence Router.
pub struct Crossroads<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Crossroads<'a> {
    /// Create a new Crossroads instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Find `k` maximally diverse outgoing neighbors from the given start node.
    ///
    /// This uses a greedy algorithm:
    /// 1. Start with an arbitrary neighbor (or the one furthest from the start node).
    /// 2. Iteratively pick the neighbor that maximizes the minimum distance to all previously selected neighbors.
    ///
    /// # Arguments
    /// * `start_node` - The node to branch out from.
    /// * `property_key` - The vector property to use for calculating divergence (e.g., "embedding").
    /// * `k` - The number of divergent paths to select.
    pub fn find_divergent_paths(
        &self,
        start_node: NodeId,
        property_key: &str,
        k: usize,
    ) -> Result<Vec<NodeId>> {
        if k == 0 {
            return Ok(Vec::new());
        }

        let neighbors = self.db.read(|tx| {
            let edges = tx.get_outgoing_edges(start_node);
            let mut neighbors = Vec::new();
            for edge_id in edges {
                if let Ok(edge) = tx.get_edge(edge_id) {
                    neighbors.push(edge.target);
                }
            }
            Ok::<_, Error>(neighbors)
        })?;

        if neighbors.is_empty() {
            return Ok(Vec::new());
        }

        // If k is greater than or equal to the number of neighbors, just return all of them.
        if k >= neighbors.len() {
            return Ok(neighbors);
        }

        // Fetch the vectors for all neighbors
        let mut neighbor_vectors: Vec<(NodeId, Vec<f32>)> = Vec::new();
        self.db.read(|tx| {
            for neighbor_id in neighbors {
                if let Ok(node) = tx.get_node(neighbor_id) {
                    if let Some(prop) = node.properties.get(property_key) {
                        if let Some(vec) = prop.as_vector() {
                            neighbor_vectors.push((neighbor_id, vec.to_vec()));
                        }
                    }
                }
            }
            Ok::<_, Error>(())
        })?;

        if neighbor_vectors.is_empty() {
            return Ok(Vec::new());
        }

        if k >= neighbor_vectors.len() {
            return Ok(neighbor_vectors.into_iter().map(|(id, _)| id).collect());
        }

        // Greedy Max-Min distance algorithm
        let mut selected = Vec::with_capacity(k);
        let mut selected_vectors = Vec::with_capacity(k);

        // Pick the first one (arbitrarily the first in the list, though could be optimized)
        let first = neighbor_vectors.remove(0);
        selected.push(first.0);
        selected_vectors.push(first.1);

        while selected.len() < k && !neighbor_vectors.is_empty() {
            let mut best_idx = 0;
            let mut max_min_dist = f32::MIN;

            for (i, (_, candidate_vec)) in neighbor_vectors.iter().enumerate() {
                let mut min_dist_to_selected = f32::MAX;

                for selected_vec in &selected_vectors {
                    // Cosine distance = 1 - Cosine Similarity
                    let sim = cosine_similarity(candidate_vec, selected_vec).map_err(|_| {
                        Error::Vector(VectorError::DimensionMismatch {
                            expected: candidate_vec.len(),
                            actual: selected_vec.len(),
                        })
                    })?;
                    let dist = 1.0 - sim;

                    if dist < min_dist_to_selected {
                        min_dist_to_selected = dist;
                    }
                }

                if min_dist_to_selected > max_min_dist {
                    max_min_dist = min_dist_to_selected;
                    best_idx = i;
                }
            }

            // Remove the best candidate from the pool and add to selected
            let best = neighbor_vectors.remove(best_idx);
            selected.push(best.0);
            selected_vectors.push(best.1);
        }

        Ok(selected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AletheiaDB;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    /// Tests for Crossroads

    #[test]
    fn test_crossroads_divergence() -> Result<()> {
        let db = AletheiaDB::new()?;

        let start_node =
            db.write(|tx| tx.create_node("Start", PropertyMapBuilder::new().build()))?;

        // Create 3 neighbors that are highly similar (e.g. all representing "Apple")
        let similar_1 = db.write(|tx| {
            tx.create_node(
                "Concept",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[1.0, 0.0, 0.0])
                    .build(),
            )
        })?;
        let similar_2 = db.write(|tx| {
            tx.create_node(
                "Concept",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.9, 0.1, 0.0])
                    .build(),
            )
        })?;
        let similar_3 = db.write(|tx| {
            tx.create_node(
                "Concept",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.9, 0.0, 0.1])
                    .build(),
            )
        })?;

        // Create 1 neighbor that is highly divergent (orthogonal) (e.g. "Orange")
        let divergent = db.write(|tx| {
            tx.create_node(
                "Concept",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.0, 1.0, 0.0])
                    .build(),
            )
        })?;

        // Connect them all
        db.write(|tx| {
            tx.create_edge(
                start_node,
                similar_1,
                "CONNECTS",
                PropertyMapBuilder::new().build(),
            )?;
            tx.create_edge(
                start_node,
                similar_2,
                "CONNECTS",
                PropertyMapBuilder::new().build(),
            )?;
            tx.create_edge(
                start_node,
                similar_3,
                "CONNECTS",
                PropertyMapBuilder::new().build(),
            )?;
            tx.create_edge(
                start_node,
                divergent,
                "CONNECTS",
                PropertyMapBuilder::new().build(),
            )?;
            Ok::<_, Error>(())
        })?;

        let crossroads = Crossroads::new(&db);

        // If we ask for 2 divergent paths, it should pick `similar_1` (arbitrary first) and `divergent`
        // because `divergent` is the furthest from `similar_1`
        let paths = crossroads.find_divergent_paths(start_node, "embedding", 2)?;

        assert_eq!(paths.len(), 2);

        // Ensure `divergent` is in the set
        assert!(paths.contains(&divergent));

        Ok(())
    }
}
