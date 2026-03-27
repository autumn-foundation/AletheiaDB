//! Tapestry: Semantic Thread Cohesion Engine.
//!
//! "Does this thread hold together, or is it unravelling?"
//!
//! Tapestry analyzes a linear sequence of nodes (a "thread" or "path") and calculates its
//! "Semantic Cohesion". It answers questions like:
//! - "Does this chain of reasoning drift off-topic?"
//! - "Is this conversation logically consistent?"
//! - "Where does the story jump the shark?"
//!
//! # Concepts
//! - **Thread**: An ordered sequence of nodes (e.g., a path traversed through the graph).
//! - **Cohesion Score**: The average semantic similarity between consecutive nodes.
//! - **Unravelling Point**: The specific edge in the thread where the semantic similarity drops
//!   below a given threshold, indicating a harsh context switch or logical leap.
//!
//! # Example
//!
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::tapestry::{Tapestry, CohesionAnalysis};
//! use aletheiadb::core::id::NodeId;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let tapestry = Tapestry::new(&db);
//!
//! # let thread = vec![NodeId::new(1).unwrap(), NodeId::new(2).unwrap(), NodeId::new(3).unwrap()];
//! // Analyze a sequence of node IDs representing a conversation or reasoning chain
//! let analysis = tapestry.analyze_thread(&thread, "embedding")?;
//!
//! println!("Overall Cohesion: {:.2}", analysis.cohesion_score);
//!
//! if let Some((idx, score)) = analysis.unravelling_point(0.5) {
//!     println!("Thread unraveled at step {} (similarity: {:.2})", idx, score);
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::api::transaction::ReadOps;
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;
use crate::core::vector::ops;

/// Result of a thread cohesion analysis.
#[derive(Debug, Clone)]
pub struct CohesionAnalysis {
    /// The average semantic similarity between all consecutive nodes in the thread.
    pub cohesion_score: f32,
    /// The individual similarity scores between each consecutive pair.
    /// Length is `thread.len() - 1`.
    pub step_scores: Vec<f32>,
}

impl CohesionAnalysis {
    /// Finds the first point in the thread where the semantic similarity between
    /// consecutive nodes drops below the given threshold.
    ///
    /// Returns `Some((index, score))` where `index` is the index of the *second* node in the pair
    /// that caused the drop, or `None` if the thread never unraveled.
    pub fn unravelling_point(&self, threshold: f32) -> Option<(usize, f32)> {
        for (i, &score) in self.step_scores.iter().enumerate() {
            if score < threshold {
                return Some((i + 1, score)); // +1 because step_scores[i] is between node i and i+1
            }
        }
        None
    }
}

/// The Tapestry Engine for analyzing semantic threads.
pub struct Tapestry<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Tapestry<'a> {
    /// Create a new Tapestry Engine.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Analyze the semantic cohesion of an ordered sequence of nodes.
    ///
    /// # Arguments
    /// * `thread` - The ordered sequence of node IDs to analyze.
    /// * `property_name` - The vector property to use for semantic analysis.
    pub fn analyze_thread(
        &self,
        thread: &[NodeId],
        property_name: &str,
    ) -> Result<CohesionAnalysis> {
        if thread.len() < 2 {
            return Err(Error::other(
                "Thread must contain at least two nodes for cohesion analysis",
            ));
        }

        let mut vectors = Vec::with_capacity(thread.len());

        // Extract vectors
        self.db.read(|tx| {
            for &node_id in thread {
                let vec = if let Ok(node) = tx.get_node(node_id) {
                    node.get_property(property_name)
                        .and_then(|p| p.as_vector())
                        .map(|v| v.to_vec())
                } else {
                    None
                };
                vectors.push(vec);
            }
            Ok::<(), Error>(())
        })?;

        // Verify all nodes had the property
        if vectors.iter().any(|v| v.is_none()) {
            return Err(Error::other(
                "One or more nodes in the thread missing the vector property",
            ));
        }

        // Calculate step-by-step similarities
        let mut step_scores = Vec::with_capacity(thread.len() - 1);
        let mut total_score = 0.0;

        for i in 0..(thread.len() - 1) {
            let vec_a = vectors[i].as_ref().unwrap();
            let vec_b = vectors[i + 1].as_ref().unwrap();

            let sim = ops::cosine_similarity(vec_a, vec_b)?;
            step_scores.push(sim);
            total_score += sim;
        }

        let cohesion_score = total_score / step_scores.len() as f32;

        Ok(CohesionAnalysis {
            cohesion_score,
            step_scores,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_tapestry_cohesion() {
        let db = AletheiaDB::new().unwrap();
        let mut n1 = NodeId::new(0).unwrap();
        let mut n2 = NodeId::new(0).unwrap();
        let mut n3 = NodeId::new(0).unwrap();

        db.write(|tx| {
            n1 = tx.create_node(
                "Node",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[1.0, 0.0])
                    .build(),
            )?;
            n2 = tx.create_node(
                "Node",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.9, 0.1])
                    .build(),
            )?;
            n3 = tx.create_node(
                "Node",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.0, 1.0])
                    .build(),
            )?;
            Ok::<(), Error>(())
        })
        .unwrap();

        let tapestry = Tapestry::new(&db);
        let thread = vec![n1, n2, n3];

        let analysis = tapestry.analyze_thread(&thread, "vec").unwrap();

        // Between n1 and n2, similarity is high. Between n2 and n3, it's low.
        assert!(analysis.step_scores[0] > 0.8);
        assert!(analysis.step_scores[1] < 0.2);

        // It unravels at the second step (index 2 of the thread)
        let unravelling = analysis.unravelling_point(0.5);
        assert!(unravelling.is_some());
        assert_eq!(unravelling.unwrap().0, 2);
    }

    #[test]
    fn test_tapestry_errors() {
        let db = AletheiaDB::new().unwrap();
        let mut n1 = NodeId::new(0).unwrap();
        let mut n2 = NodeId::new(0).unwrap();
        db.write(|tx| {
            n1 = tx.create_node("Node", PropertyMapBuilder::new().build())?;
            n2 = tx.create_node("Node", PropertyMapBuilder::new().build())?;
            Ok::<(), Error>(())
        })
        .unwrap();

        let tapestry = Tapestry::new(&db);

        // Too short
        assert!(tapestry.analyze_thread(&[n1], "vec").is_err());

        // Missing property
        assert!(tapestry.analyze_thread(&[n1, n2], "vec").is_err());
    }
}
