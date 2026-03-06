//! Vortex: Semantic Sinkhole Detector.
//!
//! "What is consuming the conversation?"
//!
//! The Vortex Detector identifies "Semantic Sinkholes"—nodes that draw in a massive,
//! highly diverse set of concepts but emit very little outward conceptual flow.
//! These nodes represent black holes of meaning: topics that absorb distinct threads
//! and compress them into a singular point of focus.
//!
//! # Concepts
//!
//! - **Structural In-Degree**: The raw volume of incoming edges (traffic/attention).
//! - **Semantic Diversity**: How different the incoming concepts are from one another. A node
//!   that attracts 1,000 links from the exact same sub-community is popular, but not a Vortex.
//!   A node that attracts 100 links from Biology, Politics, Art, and Finance is a high-diversity Sinkhole.
//! - **Vortex Score**: The combination of structural attraction and semantic diversity. High
//!   score = a powerful Sinkhole.
//!
//! # Use Cases
//!
//! - **Trend Analysis**: Finding the "main character of the internet" today (e.g., an event
//!   that sports fans, politicians, and tech workers are all talking about).
//! - **Misinformation Tracking**: Identifying vague catch-all conspiracy theories that
//!   absorb disparate real-world events.
//! - **Product Strategy**: Finding the one feature request that unifies complaints from
//!   entirely different user segments.
//!
//! # Example
//!
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::vortex::VortexDetector;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let detector = VortexDetector::new(&db);
//!
//! // Analyze a specific node to see if it's a semantic sinkhole
//! # let node_id = db.create_node("Topic", Default::default())?;
//! let score = detector.analyze(node_id, "embedding")?;
//!
//! if score.vortex_score > 0.8 {
//!     println!("Node {} is a massive Semantic Sinkhole!", node_id);
//!     println!("It attracted {} distinct ideas.", score.incoming_count);
//! }
//! # Ok(())
//! # }
//! ```

#![allow(clippy::collapsible_if)]

use crate::AletheiaDB;
use crate::api::transaction::ReadOps;
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;
use crate::core::vector::ops;
use std::collections::HashSet;

/// The result of a Vortex analysis for a single node.
#[derive(Debug, Clone, PartialEq)]
pub struct VortexScore {
    /// The analyzed node.
    pub node_id: NodeId,
    /// The number of valid incoming vectors.
    pub incoming_count: usize,
    /// The structural in-degree (total incoming edges).
    pub structural_in_degree: usize,
    /// The calculated semantic diversity of the incoming vectors (0.0 to 1.0).
    pub semantic_diversity: f32,
    /// The final Vortex score, combining degree and diversity.
    pub vortex_score: f32,
}

impl VortexScore {
    /// Returns true if the node is considered a significant Sinkhole.
    pub fn is_sinkhole(&self) -> bool {
        self.vortex_score > 0.5 && self.incoming_count > 1
    }
}

/// Detector for finding Semantic Sinkholes.
pub struct VortexDetector<'a> {
    db: &'a AletheiaDB,
}

impl<'a> VortexDetector<'a> {
    /// Create a new VortexDetector.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Analyze a specific node to determine its Vortex score.
    ///
    /// The score is calculated by:
    /// 1. Gathering all vectors from source nodes that point *to* this node.
    /// 2. Measuring the average pairwise cosine distance between these source vectors.
    /// 3. Combining this diversity metric with a logarithmic scale of the incoming edge count.
    ///
    /// # Arguments
    ///
    /// * `node_id` - The node to analyze.
    /// * `property_name` - The vector property to use for semantic analysis.
    pub fn analyze(&self, node_id: NodeId, property_name: &str) -> Result<VortexScore> {
        let mut source_vectors = Vec::new();
        let mut unique_sources = HashSet::new();
        let mut total_incoming = 0;

        self.db.read(|tx| {
            let incoming_edges = tx.get_incoming_edges(node_id);
            total_incoming = incoming_edges.len();

            for edge_id in incoming_edges {
                if let Ok(edge) = tx.get_edge(edge_id) {
                    let source_id = edge.source;
                    // Prevent duplicate processing if multiple edges exist from the same source
                    if unique_sources.insert(source_id) {
                        if let Ok(source_node) = tx.get_node(source_id) {
                            if let Some(prop) = source_node
                                .get_property(property_name)
                                .and_then(|p| p.as_vector())
                            {
                                source_vectors.push(prop.to_vec());
                            }
                        }
                    }
                }
            }
            Ok::<(), Error>(())
        })?;

        let incoming_count = source_vectors.len();

        // If a node has 0 or 1 incoming vectors, it has no diversity by definition.
        if incoming_count < 2 {
            return Ok(VortexScore {
                node_id,
                incoming_count,
                structural_in_degree: total_incoming,
                semantic_diversity: 0.0,
                vortex_score: 0.0,
            });
        }

        // Calculate Semantic Diversity: Average pairwise cosine distance
        let mut total_distance = 0.0;
        let mut pairs = 0;

        for i in 0..incoming_count {
            for j in (i + 1)..incoming_count {
                let sim = ops::cosine_similarity(&source_vectors[i], &source_vectors[j])?;
                // Distance is 1.0 - similarity. Max distance is 2.0 (for -1.0 sim), but usually 0.0 to 1.0.
                let dist = (1.0 - sim).max(0.0);
                total_distance += dist;
                pairs += 1;
            }
        }

        let avg_distance = if pairs > 0 {
            total_distance / pairs as f32
        } else {
            0.0
        };

        // Normalize diversity. In most embedding spaces, avg dist > 0.8 is extremely diverse.
        // We'll cap it at 1.0.
        let semantic_diversity = (avg_distance).min(1.0);

        // Calculate a volume modifier. We use log10 to prevent raw popularity from completely dominating.
        // log10(10) = 1.0, log10(100) = 2.0. We normalize this loosely.
        let volume_modifier = (incoming_count as f32).log10().max(0.1); // Avoid 0 for log10(1)

        // The Vortex Score is a product of diversity and the volume modifier.
        let vortex_score = semantic_diversity * volume_modifier;

        Ok(VortexScore {
            node_id,
            incoming_count,
            structural_in_degree: total_incoming,
            semantic_diversity,
            vortex_score,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_vortex_homogeneous_sources() {
        let db = AletheiaDB::new().unwrap();

        let mut sinkhole = NodeId::new(0).unwrap();
        let mut s1 = NodeId::new(0).unwrap();
        let mut s2 = NodeId::new(0).unwrap();

        db.write(|tx| {
            sinkhole = tx
                .create_node("Topic", PropertyMapBuilder::new().build())
                .unwrap();

            // Create sources with identical vectors (no diversity)
            s1 = tx
                .create_node(
                    "Source",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[1.0, 0.0])
                        .build(),
                )
                .unwrap();

            s2 = tx
                .create_node(
                    "Source",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[1.0, 0.0])
                        .build(),
                )
                .unwrap();

            tx.create_edge(s1, sinkhole, "LINKS_TO", Default::default())
                .unwrap();
            tx.create_edge(s2, sinkhole, "LINKS_TO", Default::default())
                .unwrap();

            Ok::<(), Error>(())
        })
        .unwrap();

        let detector = VortexDetector::new(&db);
        let score = detector.analyze(sinkhole, "vec").unwrap();

        assert_eq!(score.incoming_count, 2);
        // Identical vectors mean 0 distance, 0 diversity.
        assert!(score.semantic_diversity < 0.001);
        assert!(score.vortex_score < 0.001);
        assert!(!score.is_sinkhole());
    }

    #[test]
    fn test_vortex_diverse_sources() {
        let db = AletheiaDB::new().unwrap();

        let mut sinkhole = NodeId::new(0).unwrap();
        let mut s1 = NodeId::new(0).unwrap();
        let mut s2 = NodeId::new(0).unwrap();
        let mut s3 = NodeId::new(0).unwrap();

        db.write(|tx| {
            sinkhole = tx
                .create_node("Topic", PropertyMapBuilder::new().build())
                .unwrap();

            // Create sources with highly orthogonal vectors
            s1 = tx
                .create_node(
                    "Source",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[1.0, 0.0, 0.0])
                        .build(),
                )
                .unwrap();

            s2 = tx
                .create_node(
                    "Source",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[0.0, 1.0, 0.0])
                        .build(),
                )
                .unwrap();

            s3 = tx
                .create_node(
                    "Source",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[0.0, 0.0, 1.0])
                        .build(),
                )
                .unwrap();

            tx.create_edge(s1, sinkhole, "LINKS_TO", Default::default())
                .unwrap();
            tx.create_edge(s2, sinkhole, "LINKS_TO", Default::default())
                .unwrap();
            tx.create_edge(s3, sinkhole, "LINKS_TO", Default::default())
                .unwrap();

            Ok::<(), Error>(())
        })
        .unwrap();

        let detector = VortexDetector::new(&db);
        let score = detector.analyze(sinkhole, "vec").unwrap();

        assert_eq!(score.incoming_count, 3);
        // Orthogonal vectors have high distance, so diversity should be high
        assert!(score.semantic_diversity > 0.5);
        assert!(score.vortex_score > 0.2); // log10(3) * high_div
    }

    #[test]
    fn test_vortex_insufficient_sources() {
        let db = AletheiaDB::new().unwrap();

        let mut sinkhole = NodeId::new(0).unwrap();
        let mut s1 = NodeId::new(0).unwrap();

        db.write(|tx| {
            sinkhole = tx
                .create_node("Topic", PropertyMapBuilder::new().build())
                .unwrap();

            s1 = tx
                .create_node(
                    "Source",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[1.0, 0.0])
                        .build(),
                )
                .unwrap();

            tx.create_edge(s1, sinkhole, "LINKS_TO", Default::default())
                .unwrap();

            Ok::<(), Error>(())
        })
        .unwrap();

        let detector = VortexDetector::new(&db);
        let score = detector.analyze(sinkhole, "vec").unwrap();

        assert_eq!(score.incoming_count, 1);
        assert_eq!(score.semantic_diversity, 0.0);
        assert_eq!(score.vortex_score, 0.0);
    }
}
