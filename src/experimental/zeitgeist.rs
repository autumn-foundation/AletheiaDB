use crate::core::error::{Error, Result};
use crate::core::id::NodeId;
use crate::core::temporal::Timestamp;
use crate::core::vector::ops::{cosine_similarity, normalize_in_place};
use crate::db::AletheiaDB;

/// Zeitgeist: Global Semantic Drift Analysis.
///
/// Zeitgeist computes the "semantic centroid" of a set of nodes at a specific
/// point in time, allowing you to track how the overall semantic direction of
/// the data shifts ("drifts") across time.
pub struct Zeitgeist<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Zeitgeist<'a> {
    /// Create a new Zeitgeist analyzer.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Compute the semantic centroid of a set of nodes at a specific timestamp.
    ///
    /// The centroid is the average of all the valid vector properties found.
    pub fn compute_centroid_at_time(
        &self,
        nodes: &[NodeId],
        property_name: &str,
        time: Timestamp,
    ) -> Result<Vec<f32>> {
        let mut count = 0;
        let mut sum: Option<Vec<f32>> = None;

        for &node_id in nodes {
            // Retrieve node exactly as it existed at 'time'
            if let Ok(node) = self.db.get_node_at_time(node_id, time, time)
                && let Some(prop) = node.get_property(property_name)
                && let Some(vec) = prop.as_vector()
            {
                count += 1;
                match &mut sum {
                    Some(s) => {
                        // Add to existing sum
                        for (i, v) in vec.iter().enumerate() {
                            s[i] += v;
                        }
                    }
                    None => {
                        // Initialize sum with the first vector found
                        sum = Some(vec.to_vec());
                    }
                }
            }
        }

        if count == 0 {
            return Err(Error::other(format!(
                "No vectors found for property '{}' at time {}",
                property_name, time
            )));
        }

        let mut centroid = sum.unwrap();
        let count_f32 = count as f32;
        for v in &mut centroid {
            *v /= count_f32;
        }

        Ok(centroid)
    }

    /// Analyze the semantic drift between two timestamps.
    ///
    /// Computes the semantic centroid at `t1` and `t2`, normalizes both,
    /// and returns the cosine distance (1.0 - cosine_similarity).
    pub fn analyze_semantic_drift(
        &self,
        nodes: &[NodeId],
        property_name: &str,
        t1: Timestamp,
        t2: Timestamp,
    ) -> Result<f32> {
        let mut centroid1 = self.compute_centroid_at_time(nodes, property_name, t1)?;
        let mut centroid2 = self.compute_centroid_at_time(nodes, property_name, t2)?;

        normalize_in_place(&mut centroid1);
        normalize_in_place(&mut centroid2);

        let similarity = cosine_similarity(&centroid1, &centroid2)?;
        Ok((1.0_f32 - similarity).max(0.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;
    use crate::core::temporal::time::now;

    #[test]
    fn test_compute_centroid() {
        let db = AletheiaDB::new().unwrap();

        let n1 = db
            .write(|tx| {
                tx.create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[1.0, 0.0])
                        .build(),
                )
            })
            .unwrap();

        let n2 = db
            .write(|tx| {
                tx.create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[0.0, 1.0])
                        .build(),
                )
            })
            .unwrap();

        let current_time = now();
        let zeitgeist = Zeitgeist::new(&db);
        let centroid = zeitgeist
            .compute_centroid_at_time(&[n1, n2], "vec", current_time)
            .unwrap();

        assert_eq!(centroid, vec![0.5, 0.5]);
    }

    #[test]
    fn test_semantic_drift() {
        let db = AletheiaDB::new().unwrap();

        // 1. Initial State
        let node_id = db
            .write(|tx| {
                tx.create_node(
                    "Trend",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[1.0, 0.0])
                        .build(),
                )
            })
            .unwrap();

        let t1 = now();

        // Wait a bit to ensure distinct timestamp
        std::thread::sleep(std::time::Duration::from_millis(5));

        // 2. Updated State
        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.0, 1.0])
                    .build(),
            )
        })
        .unwrap();

        let t2 = now();

        let zeitgeist = Zeitgeist::new(&db);
        let drift = zeitgeist
            .analyze_semantic_drift(&[node_id], "embedding", t1, t2)
            .unwrap();

        // Vectors [1, 0] and [0, 1] are orthogonal.
        // Cosine similarity = 0.0, so drift (1.0 - similarity) should be ~1.0
        assert!(drift > 0.99 && drift <= 1.0);
    }
}
