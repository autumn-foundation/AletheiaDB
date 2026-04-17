//! Geiger: Semantic Radiation Detector ☢️
//!
//! "Find the glowing anomalies in your graph."
//!
//! Geiger detects "radioactive" nodes—nodes whose semantic vectors
//! drastically diverge from their structural neighborhood. In a graph
//! where connections usually imply similarity (homophily), these nodes
//! are anomalies.
//!
//! # The Hook
//! "A user is connected to 50 'Gaming' communities, but their vector represents
//! 'Financial Phishing'. Geiger spots them instantly."
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::geiger::Geiger;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let geiger = Geiger::new(&db);
//!
//! // Scan a node to get its radiation level (0.0 to 1.0)
//! # let node_id = aletheiadb::core::id::NodeId::new(1).unwrap();
//! let radiation = geiger.scan_node(node_id, "embedding")?;
//! if radiation > 0.8 {
//!     println!("⚠️ Highly anomalous node detected!");
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::api::ReadOps;
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;

/// A detector for semantic anomalies in graph neighborhoods.
pub struct Geiger<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Geiger<'a> {
    /// Creates a new Geiger detector.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Scans a node to determine its semantic radiation level (0.0 to 1.0).
    ///
    /// The radiation level is calculated as `1.0 - max(0.0, cosine_similarity)`.
    /// - `0.0` means the node is perfectly aligned with its neighborhood.
    /// - `1.0` means the node is completely orthogonal or opposite to its neighborhood.
    ///
    /// If the node has no neighbors with vectors, returns `0.0`.
    pub fn scan_node(&self, node_id: NodeId, vector_property: &str) -> Result<f32> {
        let tx = self.db.read_transaction()?;

        // Get the target node's vector
        let target_node = tx.get_node(node_id)?;
        let target_vec = match target_node.properties.get(vector_property) {
            Some(val) => val
                .as_vector()
                .ok_or_else(|| Error::other("Property is not a vector"))?,
            None => return Err(Error::other("Node missing vector property")),
        };

        // Get neighbors (both incoming and outgoing)
        let mut edges = tx.get_outgoing_edges(node_id);
        edges.extend(tx.get_incoming_edges(node_id));
        let mut neighbor_vectors = Vec::new();

        for edge_id in edges {
            if let Ok(edge) = tx.get_edge(edge_id) {
                let neighbor_id = if edge.source == node_id {
                    edge.target
                } else {
                    edge.source
                };
                if let Ok(neighbor) = tx.get_node(neighbor_id) {
                    #[allow(clippy::collapsible_if)]
                    if let Some(vec) = neighbor
                        .properties
                        .get(vector_property)
                        .and_then(|v| v.as_vector())
                    {
                        neighbor_vectors.push(vec.to_vec());
                    }
                }
            }
        }

        if neighbor_vectors.is_empty() {
            return Ok(0.0); // No radiation if no neighbors to compare against
        }

        // Calculate neighborhood centroid
        let dims = target_vec.len();
        let mut centroid = vec![0.0; dims];
        for vec in &neighbor_vectors {
            if vec.len() != dims {
                continue; // Skip mismatched dimensions
            }
            for i in 0..dims {
                centroid[i] += vec[i];
            }
        }

        let count = neighbor_vectors.len() as f32;
        for c in centroid.iter_mut().take(dims) {
            *c /= count;
        }

        // Calculate cosine similarity
        let sim = cosine_similarity(target_vec, &centroid);

        // Radiation is the inverse of similarity (clamped to 0.0 - 1.0)
        let radiation = 1.0 - sim.max(0.0);
        Ok(radiation.min(1.0))
    }
}

// Helper for cosine similarity
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }

    let mut dot = 0.0;
    let mut norm_a = 0.0;
    let mut norm_b = 0.0;

    for i in 0..a.len() {
        dot += a[i] * b[i];
        norm_a += a[i] * a[i];
        norm_b += b[i] * b[i];
    }

    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }

    dot / (norm_a.sqrt() * norm_b.sqrt())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_geiger_radiation() -> Result<()> {
        let db = AletheiaDB::new()?;

        let mut tx = db.write_transaction()?;

        // Normal nodes (similar vectors)
        let v1 = vec![1.0, 0.0, 0.0];
        let v2 = vec![0.9, 0.1, 0.0];
        let v3 = vec![0.8, 0.2, 0.0];

        // Anomalous node (orthogonal vector)
        let v_anomaly = vec![0.0, 1.0, 0.0];

        let n1 = tx.create_node(
            "Normie",
            PropertyMapBuilder::new().insert("vec", v1.clone()).build(),
        )?;
        let n2 = tx.create_node(
            "Normie",
            PropertyMapBuilder::new().insert("vec", v2).build(),
        )?;
        let n3 = tx.create_node(
            "Normie",
            PropertyMapBuilder::new().insert("vec", v3).build(),
        )?;
        let anomaly = tx.create_node(
            "Anomaly",
            PropertyMapBuilder::new().insert("vec", v_anomaly).build(),
        )?;

        // Connect them: N1 is connected to N2 and N3
        tx.create_edge(n1, n2, "KNOWS", PropertyMapBuilder::new().build())?;
        tx.create_edge(n1, n3, "KNOWS", PropertyMapBuilder::new().build())?;

        // Connect anomaly to N2 and N3
        tx.create_edge(anomaly, n2, "KNOWS", PropertyMapBuilder::new().build())?;
        tx.create_edge(anomaly, n3, "KNOWS", PropertyMapBuilder::new().build())?;

        tx.commit()?;

        let geiger = Geiger::new(&db);

        let rad_n1 = geiger.scan_node(n1, "vec")?;
        let rad_anomaly = geiger.scan_node(anomaly, "vec")?;

        // N1 should have low radiation (very similar to neighbors)
        assert!(rad_n1 < 0.2, "Radiation was {}", rad_n1);

        // Anomaly should have high radiation (orthogonal to neighbors)
        assert!(rad_anomaly > 0.8, "Radiation was {}", rad_anomaly);

        Ok(())
    }
}
