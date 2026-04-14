//! Polygraph: Semantic-Structural Consistency Checker.
//!
//! "Do the edges match the semantics?"
//!
//! Graphs have explicitly defined edges (Structure) and implicit meaning (Vectors).
//! Polygraph detects when they contradict each other. For example, if two nodes are connected
//! by an 'IS_SIMILAR_TO' edge but their vectors are opposites, Polygraph flags it as a lie.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::polygraph::{Polygraph, PolygraphRule};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let mut polygraph = Polygraph::new(&db);
//!
//! // "FRIEND" edges should mean the nodes are semantically similar (> 0.8)
//! polygraph.add_rule("FRIEND", PolygraphRule::RequiresHighSimilarity(0.8));
//! // "ENEMY" edges should mean they are semantically distant (< 0.2)
//! polygraph.add_rule("ENEMY", PolygraphRule::RequiresLowSimilarity(0.2));
//!
//! let lies = polygraph.investigate("embedding")?;
//! for lie in lies {
//!     println!("Detected contradiction on edge {}!", lie.edge_id);
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::{EdgeId, NodeId};
use crate::core::vector::ops::cosine_similarity;
use std::collections::HashMap;

/// Rules for semantic consistency on an edge.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PolygraphRule {
    /// The edge implies the nodes should be semantically similar (cosine similarity >= threshold).
    RequiresHighSimilarity(f32),
    /// The edge implies the nodes should be semantically distant (cosine similarity <= threshold).
    RequiresLowSimilarity(f32),
}

/// A detected contradiction between graph structure and vector semantics.
#[derive(Debug, Clone, PartialEq)]
pub struct Contradiction {
    /// The edge that violates the rule.
    pub edge_id: EdgeId,
    /// The source node.
    pub source: NodeId,
    /// The target node.
    pub target: NodeId,
    /// The edge label.
    pub label: String,
    /// The actual cosine similarity between source and target.
    pub actual_similarity: f32,
    /// The rule that was violated.
    pub violated_rule: PolygraphRule,
}

/// The Polygraph engine for detecting semantic lies.
pub struct Polygraph<'a> {
    db: &'a AletheiaDB,
    rules: HashMap<String, PolygraphRule>,
}

#[allow(clippy::collapsible_if)]
impl<'a> Polygraph<'a> {
    /// Create a new Polygraph instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self {
            db,
            rules: HashMap::new(),
        }
    }

    /// Add a semantic consistency rule for a specific edge label.
    pub fn add_rule(&mut self, edge_label: &str, rule: PolygraphRule) {
        self.rules.insert(edge_label.to_string(), rule);
    }

    /// Investigate the entire graph for semantic contradictions.
    ///
    /// # Arguments
    /// * `vector_property` - The property name containing the node embeddings.
    pub fn investigate(&self, vector_property: &str) -> Result<Vec<Contradiction>> {
        let mut contradictions = Vec::new();

        if self.rules.is_empty() {
            return Ok(contradictions);
        }

        // 1. Scan all nodes to find outgoing edges
        let scan_results = self.db.query().scan(None).execute(self.db)?;

        for row_result in scan_results {
            let row = row_result?;
            if let Some(node) = row.entity.as_node() {
                // Get vector for source node if available
                let source_vec_opt = node
                    .get_property(vector_property)
                    .and_then(|p| p.as_vector())
                    .map(|v| v.to_vec());

                // If source has no vector, it can't violate similarity rules
                let source_vec = match source_vec_opt {
                    Some(v) => v,
                    None => continue,
                };

                let outgoing_edges = self.db.get_outgoing_edges(node.id);
                for edge_id in outgoing_edges {
                    if let Ok(edge) = self.db.get_edge(edge_id) {
                        // Check if we have a rule for this edge's label
                        if let Some(&rule) = self.rules.get(edge.label.to_string().as_str()) {
                            // Fetch target node to get its vector
                            if let Ok(target_node) = self.db.get_node(edge.target) {
                                if let Some(target_vec) = target_node
                                    .properties
                                    .get(vector_property)
                                    .and_then(|p| p.as_vector())
                                {
                                    let sim = cosine_similarity(&source_vec, target_vec)?;

                                    let is_violation = match rule {
                                        PolygraphRule::RequiresHighSimilarity(threshold) => {
                                            sim < threshold
                                        }
                                        PolygraphRule::RequiresLowSimilarity(threshold) => {
                                            sim > threshold
                                        }
                                    };

                                    if is_violation {
                                        contradictions.push(Contradiction {
                                            edge_id,
                                            source: node.id,
                                            target: edge.target,
                                            label: edge.label.to_string(),
                                            actual_similarity: sim,
                                            violated_rule: rule,
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(contradictions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_polygraph_detects_lies() {
        let db = AletheiaDB::new().unwrap();

        // Node A: [1.0, 0.0]
        let a = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[1.0, 0.0])
                    .build(),
            )
            .unwrap();

        // Node B: [0.9, 0.1] (Very similar to A)
        let b = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.9, 0.1])
                    .build(),
            )
            .unwrap();

        // Node C: [-1.0, 0.0] (Opposite to A)
        let c = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[-1.0, 0.0])
                    .build(),
            )
            .unwrap();

        // Create edges
        // True: A and B are similar, labeled FRIEND.
        db.create_edge(a, b, "FRIEND", Default::default()).unwrap();
        // Lie: A and C are opposites, but labeled FRIEND!
        let lie_edge = db.create_edge(a, c, "FRIEND", Default::default()).unwrap();
        // Lie: A and B are similar, but labeled ENEMY!
        let lie_edge_2 = db.create_edge(a, b, "ENEMY", Default::default()).unwrap();
        // True: A and C are opposites, labeled ENEMY.
        db.create_edge(a, c, "ENEMY", Default::default()).unwrap();

        let mut polygraph = Polygraph::new(&db);
        polygraph.add_rule("FRIEND", PolygraphRule::RequiresHighSimilarity(0.8));
        polygraph.add_rule("ENEMY", PolygraphRule::RequiresLowSimilarity(0.2));

        let contradictions = polygraph.investigate("vec").unwrap();

        assert_eq!(contradictions.len(), 2);

        let has_friend_lie = contradictions
            .iter()
            .any(|c| c.edge_id == lie_edge && c.label == "FRIEND");
        let has_enemy_lie = contradictions
            .iter()
            .any(|c| c.edge_id == lie_edge_2 && c.label == "ENEMY");

        assert!(has_friend_lie);
        assert!(has_enemy_lie);
    }
}
