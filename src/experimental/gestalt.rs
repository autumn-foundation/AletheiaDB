//! Gestalt: Semantic Subgraph Matching Engine.
//!
//! "The whole is greater than the sum of its parts."
//!
//! Gestalt finds occurrences of a specific graph pattern where nodes are matched
//! not just by label, but by **semantic similarity** to a concept vector.
//!
//! This enables queries like:
//! "Find a \[Person ~ 'Engineer'\] connected to a \[Company ~ 'Startup'\] via \[WORKS_FOR\]."
//!
//! # Concepts
//! - **Pattern**: A template subgraph with constraints.
//! - **Anchor**: A node in the pattern with a vector constraint, used to seed the search.
//! - **Match**: A concrete subgraph in the database that satisfies the pattern.

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::{EdgeId, NodeId};
use std::collections::{HashMap, HashSet};

/// A node in the pattern graph.
#[derive(Debug, Clone)]
pub struct PatternNode {
    /// The ID of the node within the pattern (0, 1, 2...).
    pub id: usize,
    /// Optional label constraint (exact match).
    pub label: Option<String>,
    /// Optional vector constraint (similarity search).
    pub vector_constraint: Option<VectorConstraint>,
}

/// A vector constraint on a pattern node.
#[derive(Debug, Clone)]
pub struct VectorConstraint {
    /// The property containing the vector.
    pub property: String,
    /// The target vector concept.
    pub vector: Vec<f32>,
    /// Minimum similarity threshold (0.0 to 1.0).
    pub threshold: f32,
}

/// An edge in the pattern graph.
#[derive(Debug, Clone)]
pub struct PatternEdge {
    /// The source pattern node ID.
    pub source: usize,
    /// The target pattern node ID.
    pub target: usize,
    /// Optional label constraint (exact match).
    pub label: Option<String>,
    /// Is the edge directed? (Currently only directed supported).
    pub directed: bool,
}

/// A graph pattern to search for.
#[derive(Debug, Clone, Default)]
pub struct Pattern {
    /// Nodes in the pattern.
    pub nodes: Vec<PatternNode>,
    /// Edges in the pattern.
    pub edges: Vec<PatternEdge>,
}

impl Pattern {
    /// Create a new empty pattern.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a node to the pattern.
    pub fn add_node(&mut self, label: Option<String>) -> usize {
        let id = self.nodes.len();
        self.nodes.push(PatternNode {
            id,
            label,
            vector_constraint: None,
        });
        id
    }

    /// Add a node with a vector constraint.
    pub fn add_semantic_node(
        &mut self,
        label: Option<String>,
        property: String,
        vector: Vec<f32>,
        threshold: f32,
    ) -> usize {
        let id = self.nodes.len();
        self.nodes.push(PatternNode {
            id,
            label,
            vector_constraint: Some(VectorConstraint {
                property,
                vector,
                threshold,
            }),
        });
        id
    }

    /// Add a directed edge.
    pub fn add_edge(&mut self, source: usize, target: usize, label: Option<String>) {
        self.edges.push(PatternEdge {
            source,
            target,
            label,
            directed: true,
        });
    }

    /// Validate the pattern (e.g., check bounds).
    fn validate(&self) -> Result<()> {
        let node_count = self.nodes.len();
        for edge in &self.edges {
            if edge.source >= node_count || edge.target >= node_count {
                return Err(crate::core::error::Error::other(format!(
                    "Pattern edge references invalid node ID: {} -> {}",
                    edge.source, edge.target
                )));
            }
        }
        Ok(())
    }
}

/// A concrete match found in the database.
#[derive(Debug, Clone)]
pub struct Match {
    /// Mapping from Pattern Node ID -> Database Node ID.
    pub nodes: HashMap<usize, NodeId>,
    /// Mapping from Pattern Edge (index) -> Database Edge ID.
    pub edges: HashMap<usize, EdgeId>,
    /// The average similarity score of all vector constraints in this match.
    pub score: f32,
}

/// The Gestalt Engine.
pub struct GestaltMatcher<'a> {
    db: &'a AletheiaDB,
}

impl<'a> GestaltMatcher<'a> {
    /// Create a new matcher.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Find occurrences of the pattern in the database.
    pub fn find_matches(&self, pattern: &Pattern, limit: usize) -> Result<Vec<Match>> {
        pattern.validate()?;

        if pattern.nodes.is_empty() {
            return Ok(Vec::new());
        }

        // 1. Select Anchor
        // We pick the node with a vector constraint as it allows efficient indexing.
        // If multiple, pick the first one.
        // If none, we fall back to label scan (not implemented efficiently for MVP, scan all nodes with label).
        let anchor_idx = pattern
            .nodes
            .iter()
            .position(|n| n.vector_constraint.is_some())
            .ok_or_else(|| {
                crate::core::error::Error::other(
                    "Gestalt requires at least one node with a vector constraint to serve as an anchor.",
                )
            })?;

        let anchor_node = &pattern.nodes[anchor_idx];
        let constraint = anchor_node.vector_constraint.as_ref().unwrap();

        // 2. Find Candidates for Anchor
        // We search for nodes similar to the constraint vector.
        // We fetch more than `limit` to allow for pruning during expansion.
        // Let's fetch limit * 10 or a fixed pool.
        let search_k = limit * 10;
        let candidates =
            self.db
                .search_vectors_in(&constraint.property, &constraint.vector, search_k)?;

        let mut matches = Vec::new();

        // 3. Expand from Candidates
        for (candidate_id, score) in candidates {
            if score < constraint.threshold {
                continue; // Threshold check
            }

            // Also check label constraint on anchor
            if !self.check_node_label(candidate_id, &anchor_node.label)? {
                continue;
            }

            // Start backtracking search
            let mut current_match = Match {
                nodes: HashMap::new(),
                edges: HashMap::new(),
                score: 0.0, // Will recalculate
            };
            current_match.nodes.insert(anchor_idx, candidate_id);

            // We need to visit all other nodes in the pattern.
            // We use a recursive helper.
            self.backtrack(pattern, &mut current_match, &mut matches, limit)?;

            if matches.len() >= limit {
                break;
            }
        }

        Ok(matches)
    }

    // Recursive backtracking to complete the match.
    fn backtrack(
        &self,
        pattern: &Pattern,
        current_match: &mut Match,
        results: &mut Vec<Match>,
        limit: usize,
    ) -> Result<()> {
        if results.len() >= limit {
            return Ok(());
        }

        // Check if match is complete (all nodes mapped)
        if current_match.nodes.len() == pattern.nodes.len() {
            // Verify all edges exist between mapped nodes
            if self.verify_edges(pattern, current_match)? {
                // Calculate score
                let score = self.calculate_score(pattern, current_match)?;
                let mut final_match = current_match.clone();
                final_match.score = score;
                results.push(final_match);
            }
            return Ok(());
        }

        // Pick next unmapped node that is connected to an already mapped node
        // This ensures we traverse connected components.
        let next_node_idx = self.pick_next_node(pattern, current_match);

        if let Some(next_idx) = next_node_idx {
            // Find candidates for this node
            // Candidates must be neighbors of the mapped nodes that connect to it.
            // 1. Identify constraints from existing mappings.
            // E.g. Pattern: 0 -> 1. 0 is mapped to A. 1 must be a neighbor of A.

            let candidates = self.find_candidates_for_node(pattern, next_idx, current_match)?;

            for candidate_id in candidates {
                // Check if candidate is already used in this match (Bijectivity)
                if current_match.nodes.values().any(|&id| id == candidate_id) {
                    continue;
                }

                // Check node constraints (Label + Vector)
                let p_node = &pattern.nodes[next_idx];
                if !self.check_node_constraints(candidate_id, p_node)? {
                    continue;
                }

                // Tentatively map
                current_match.nodes.insert(next_idx, candidate_id);

                // Recurse
                self.backtrack(pattern, current_match, results, limit)?;

                // Backtrack
                current_match.nodes.remove(&next_idx);

                if results.len() >= limit {
                    return Ok(());
                }
            }
        } else {
            // Pattern might be disconnected?
            // If we have unmapped nodes but can't reach them from mapped ones,
            // we'd need to pick a new anchor for the disconnected component.
            // For MVP, we assume connected patterns or fail.
            // Or we just scan all nodes (expensive).
            // Let's stop here.
        }

        Ok(())
    }

    fn pick_next_node(&self, pattern: &Pattern, current_match: &Match) -> Option<usize> {
        // Find an unmapped pattern node that is connected to a mapped pattern node
        for edge in &pattern.edges {
            let s_mapped = current_match.nodes.contains_key(&edge.source);
            let t_mapped = current_match.nodes.contains_key(&edge.target);

            if s_mapped && !t_mapped {
                return Some(edge.target);
            }
            if !s_mapped && t_mapped {
                return Some(edge.source);
            }
        }
        None
    }

    fn find_candidates_for_node(
        &self,
        pattern: &Pattern,
        target_idx: usize,
        current_match: &Match,
    ) -> Result<Vec<NodeId>> {
        let mut candidates = HashSet::new();
        let mut first = true;

        // Look at all edges connecting to target_idx from already mapped nodes
        for edge in &pattern.edges {
            // Incoming edge: Source (Mapped) -> Target (To Map)
            if edge.target == target_idx && current_match.nodes.contains_key(&edge.source) {
                let source_db_id = current_match.nodes[&edge.source];
                let neighbors = self.get_outgoing_neighbors(source_db_id, &edge.label)?;

                if first {
                    candidates = neighbors;
                    first = false;
                } else {
                    candidates.retain(|id| neighbors.contains(id));
                }
            }

            // Outgoing edge: Target (Mapped) <- Source (To Map)
            // (If pattern edge is Source -> Target, and we are mapping Source, and Target is mapped)
            if edge.source == target_idx && current_match.nodes.contains_key(&edge.target) {
                let target_db_id = current_match.nodes[&edge.target];
                let neighbors = self.get_incoming_neighbors(target_db_id, &edge.label)?;

                if first {
                    candidates = neighbors;
                    first = false;
                } else {
                    candidates.retain(|id| neighbors.contains(id));
                }
            }
        }

        if first {
            // No connected mapped nodes? Should be handled by pick_next_node logic (disconnected graph).
            return Ok(Vec::new());
        }

        Ok(candidates.into_iter().collect())
    }

    fn get_outgoing_neighbors(
        &self,
        source: NodeId,
        label: &Option<String>,
    ) -> Result<HashSet<NodeId>> {
        let mut neighbors = HashSet::new();
        let edges = if let Some(l) = label {
            self.db.get_outgoing_edges_with_label(source, l)
        } else {
            self.db.get_outgoing_edges(source)
        };

        for edge_id in edges {
            let target = self.db.get_edge_target(edge_id)?;
            neighbors.insert(target);
        }
        Ok(neighbors)
    }

    fn get_incoming_neighbors(
        &self,
        target: NodeId,
        label: &Option<String>,
    ) -> Result<HashSet<NodeId>> {
        let mut neighbors = HashSet::new();
        let edges = if let Some(l) = label {
            self.db.current.get_incoming_edges_with_label(target, l)
        } else {
            self.db.get_incoming_edges(target)
        };

        for edge_id in edges {
            let source = self.db.get_edge_source(edge_id)?;
            neighbors.insert(source);
        }
        Ok(neighbors)
    }

    fn check_node_label(&self, node_id: NodeId, label: &Option<String>) -> Result<bool> {
        if let Some(l) = label {
            let node = self.db.get_node(node_id)?;
            // Check label
            // Node has interned label. Need to resolve or intern input.
            // Easier to check node.has_label_str
            Ok(node.has_label_str(l))
        } else {
            Ok(true)
        }
    }

    fn check_node_constraints(&self, node_id: NodeId, pattern_node: &PatternNode) -> Result<bool> {
        // Label check
        if !self.check_node_label(node_id, &pattern_node.label)? {
            return Ok(false);
        }

        // Vector check
        if let Some(vc) = &pattern_node.vector_constraint {
            let node = self.db.get_node(node_id)?;
            if let Some(val) = node.properties.get(&vc.property) {
                if let Some(vec) = val.as_vector() {
                    let sim = crate::core::vector::cosine_similarity(vec, &vc.vector)?;
                    if sim < vc.threshold {
                        return Ok(false);
                    }
                } else {
                    return Ok(false); // Wrong type
                }
            } else {
                return Ok(false); // Missing property
            }
        }

        Ok(true)
    }

    fn verify_edges(&self, pattern: &Pattern, current_match: &mut Match) -> Result<bool> {
        // Ensure all pattern edges exist in the database between the mapped nodes
        for (i, edge) in pattern.edges.iter().enumerate() {
            let u = current_match.nodes[&edge.source];
            let v = current_match.nodes[&edge.target];

            // Find edge from u to v with label
            let outgoing = if let Some(l) = &edge.label {
                self.db.get_outgoing_edges_with_label(u, l)
            } else {
                self.db.get_outgoing_edges(u)
            };

            let mut found = false;
            for edge_id in outgoing {
                if self.db.get_edge_target(edge_id)? == v {
                    current_match.edges.insert(i, edge_id);
                    found = true;
                    break;
                }
            }

            if !found {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn calculate_score(&self, pattern: &Pattern, current_match: &Match) -> Result<f32> {
        let mut total_score = 0.0;
        let mut count = 0;

        for node_idx in 0..pattern.nodes.len() {
            if let Some(vc) = &pattern.nodes[node_idx].vector_constraint {
                let db_id = current_match.nodes[&node_idx];
                let node = self.db.get_node(db_id)?;
                if let Some(vec) = node
                    .properties
                    .get(&vc.property)
                    .and_then(|v| v.as_vector())
                {
                    let sim = crate::core::vector::cosine_similarity(vec, &vc.vector)?;
                    total_score += sim;
                    count += 1;
                }
            }
        }

        if count > 0 {
            Ok(total_score / count as f32)
        } else {
            Ok(1.0) // Perfect match if no vector constraints?
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;
    use crate::index::vector::{DistanceMetric, HnswConfig};

    #[test]
    fn test_gestalt_simple_match() {
        let db = AletheiaDB::new().unwrap();
        // Enable vector index
        db.enable_vector_index("vec", HnswConfig::new(2, DistanceMetric::Cosine))
            .unwrap();

        // Graph:
        // A(Person, [1,0]) -> WORKS_FOR -> B(Company, [0,1])
        // C(Person, [1,0]) -> WORKS_FOR -> D(Company, [0,1])
        // E(Person, [0,1]) -> WORKS_FOR -> F(Company, [1,0]) (Mismatch)

        let props_a = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 0.0])
            .build();
        let a = db.create_node("Person", props_a).unwrap();

        let props_b = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 1.0])
            .build();
        let b = db.create_node("Company", props_b).unwrap();
        db.create_edge(a, b, "WORKS_FOR", Default::default())
            .unwrap();

        let props_c = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 0.0])
            .build();
        let c = db.create_node("Person", props_c).unwrap();

        let props_d = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 1.0])
            .build();
        let d = db.create_node("Company", props_d).unwrap();
        db.create_edge(c, d, "WORKS_FOR", Default::default())
            .unwrap();

        let props_e = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 1.0]) // Mismatch
            .build();
        let e = db.create_node("Person", props_e).unwrap();

        let props_f = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 0.0]) // Mismatch
            .build();
        let f = db.create_node("Company", props_f).unwrap();
        db.create_edge(e, f, "WORKS_FOR", Default::default())
            .unwrap();

        // Pattern:
        // Node 0: [1, 0] (Sim > 0.9)
        // Node 1: [0, 1] (Sim > 0.9)
        // Edge 0 -> 1 (WORKS_FOR)

        let mut pattern = Pattern::new();
        let p0 = pattern.add_semantic_node(
            Some("Person".to_string()),
            "vec".to_string(),
            vec![1.0, 0.0],
            0.9,
        );
        let p1 = pattern.add_semantic_node(
            Some("Company".to_string()),
            "vec".to_string(),
            vec![0.0, 1.0],
            0.9,
        );
        pattern.add_edge(p0, p1, Some("WORKS_FOR".to_string()));

        let matcher = GestaltMatcher::new(&db);
        let matches = matcher.find_matches(&pattern, 10).unwrap();

        // Expect 2 matches (A->B, C->D)
        assert_eq!(matches.len(), 2);

        // Verify content
        let found_pairs: Vec<(NodeId, NodeId)> =
            matches.iter().map(|m| (m.nodes[&0], m.nodes[&1])).collect();

        assert!(found_pairs.contains(&(a, b)));
        assert!(found_pairs.contains(&(c, d)));
        assert!(!found_pairs.contains(&(e, f)));
    }
}
