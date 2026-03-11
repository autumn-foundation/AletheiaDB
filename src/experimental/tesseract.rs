//! Tesseract: Semantic Boundary Detector.
//!
//! "Where does one concept end and another begin?"
//!
//! Tesseract analyzes a set of nodes to discover if they contain an internal
//! semantic boundary (a gap in vector space), indicating that the nodes actually
//! represent distinct, separated sub-clusters rather than a single contiguous concept.
//!
//! # How it works
//! 1. **Poles**: It finds the two nodes furthest apart in the provided set.
//!    These act as the "poles" of the primary axis of variation.
//! 2. **Projection**: It projects all other node vectors onto this axis, reducing
//!    the high-dimensional space into a 1D spectrum.
//! 3. **Gap Detection**: It sorts the 1D projections and looks for the largest gap.
//!    If the gap is significantly larger than the average distance between points,
//!    it flags a semantic boundary.
//!
//! # Use Cases
//! - **Concept Splitting**: "Is this 'Apple' cluster actually two clusters (Fruit vs Tech)?"
//! - **Echo Chamber Detection**: "Are the users in this group totally polarized?"
//! - **Data Quality**: Finding discontinuities in what should be smooth semantic gradients.

use crate::AletheiaDB;
use crate::api::transaction::ReadOps;
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;

/// The result of a Tesseract boundary analysis.
#[derive(Debug, Clone)]
pub struct BoundaryScore {
    /// The size of the largest gap detected on the primary axis.
    pub max_gap: f32,
    /// The average gap size (excluding the max gap).
    pub avg_gap: f32,
    /// Ratio of max_gap to avg_gap. High score (> 2.0) usually means a real boundary.
    pub boundary_score: f32,
    /// The two nodes that define the primary axis.
    pub poles: (NodeId, NodeId),
    /// The total distance between the poles.
    pub axis_length: f32,
}

impl BoundaryScore {
    /// Heuristic to determine if a significant boundary exists.
    pub fn has_boundary(&self) -> bool {
        // A boundary score > 2.5 means the largest gap is 2.5x bigger than the average gap.
        // We also want to ensure the axis itself isn't tiny (meaning all points are basically identical).
        self.boundary_score > 2.5 && self.axis_length > 0.1
    }
}

/// The Tesseract Engine.
pub struct Tesseract<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Tesseract<'a> {
    /// Create a new Tesseract instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Analyze a set of nodes for a semantic boundary.
    ///
    /// # Arguments
    /// * `nodes` - The list of node IDs to analyze. Must contain at least 3 valid vectors.
    /// * `property_name` - The vector property to use.
    pub fn analyze_boundary(&self, nodes: &[NodeId], property_name: &str) -> Result<BoundaryScore> {
        let mut vectors = Vec::new();
        let mut valid_nodes = Vec::new();

        self.db.read(|tx| {
            for &node_id in nodes {
                #[allow(clippy::collapsible_if)]
                if let Ok(node) = tx.get_node(node_id) {
                    if let Some(prop) = node.get_property(property_name).and_then(|p| p.as_vector())
                    {
                        vectors.push(prop.to_vec());
                        valid_nodes.push(node_id);
                    }
                }
            }
            Ok::<(), Error>(())
        })?;

        if valid_nodes.len() < 3 {
            return Err(Error::other(
                "Tesseract requires at least 3 nodes with vectors to detect a boundary",
            ));
        }

        // 1. Find the poles (the two furthest points)
        // O(N^2) is fine here assuming node sets aren't massive.
        // For massive sets, PCA would be better to find the principal component.
        let mut max_dist_sq = -1.0_f32;
        let mut pole1_idx = 0;
        let mut pole2_idx = 0;

        for i in 0..vectors.len() {
            for j in (i + 1)..vectors.len() {
                let dist_sq = Self::euclidean_distance_sq(&vectors[i], &vectors[j]);
                if dist_sq > max_dist_sq {
                    max_dist_sq = dist_sq;
                    pole1_idx = i;
                    pole2_idx = j;
                }
            }
        }

        let p1 = &vectors[pole1_idx];
        let p2 = &vectors[pole2_idx];
        let axis_length = max_dist_sq.sqrt();

        if axis_length < 1e-6 {
            // All points are identical
            return Ok(BoundaryScore {
                max_gap: 0.0,
                avg_gap: 0.0,
                boundary_score: 0.0,
                poles: (valid_nodes[pole1_idx], valid_nodes[pole2_idx]),
                axis_length,
            });
        }

        // Create the axis vector (p2 - p1)
        let mut axis = vec![0.0; p1.len()];
        for k in 0..p1.len() {
            axis[k] = p2[k] - p1[k];
        }

        // 2. Project all points onto the axis
        // Projection scalar t = dot(v - p1, axis) / dot(axis, axis)
        // where v is the point vector.
        let axis_dot_axis = Self::dot_product(&axis, &axis);

        let mut projections = Vec::with_capacity(vectors.len());

        for (i, v) in vectors.iter().enumerate() {
            let mut v_minus_p1 = vec![0.0; p1.len()];
            for k in 0..p1.len() {
                v_minus_p1[k] = v[k] - p1[k];
            }

            let t = Self::dot_product(&v_minus_p1, &axis) / axis_dot_axis;
            projections.push((t, valid_nodes[i]));
        }

        // 3. Sort projections along the 1D axis
        // t should roughly go from 0.0 (at p1) to 1.0 (at p2).
        projections.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

        // 4. Find the gaps
        let mut gaps = Vec::with_capacity(projections.len() - 1);
        for i in 0..(projections.len() - 1) {
            let gap = projections[i + 1].0 - projections[i].0;
            gaps.push(gap);
        }

        if gaps.is_empty() {
            return Ok(BoundaryScore {
                max_gap: 0.0,
                avg_gap: 0.0,
                boundary_score: 0.0,
                poles: (valid_nodes[pole1_idx], valid_nodes[pole2_idx]),
                axis_length,
            });
        }

        // Find max gap
        let mut max_gap_val = -1.0_f32;
        let mut max_gap_idx = 0;
        for (i, &g) in gaps.iter().enumerate() {
            if g > max_gap_val {
                max_gap_val = g;
                max_gap_idx = i;
            }
        }

        // Calculate avg gap (excluding the max gap)
        let mut sum_other_gaps = 0.0;
        let mut count_other_gaps = 0;

        for (i, &g) in gaps.iter().enumerate() {
            if i != max_gap_idx {
                sum_other_gaps += g;
                count_other_gaps += 1;
            }
        }

        let avg_gap = if count_other_gaps > 0 {
            sum_other_gaps / count_other_gaps as f32
        } else {
            // Only 2 points total (which shouldn't happen due to len < 3 check), or 3 points where one is identical
            0.0
        };

        // If the remaining points are tightly packed (avg_gap ~ 0), but max_gap is large,
        // it's a huge boundary.
        let boundary_score = if avg_gap > 1e-6 {
            max_gap_val / avg_gap
        } else if max_gap_val > 1e-6 {
            // Infinite boundary
            100.0
        } else {
            0.0
        };

        // Scale gaps back to actual distance (they were in terms of 't' [0, 1])
        let scaled_max_gap = max_gap_val * axis_length;
        let scaled_avg_gap = avg_gap * axis_length;

        Ok(BoundaryScore {
            max_gap: scaled_max_gap,
            avg_gap: scaled_avg_gap,
            boundary_score,
            poles: (valid_nodes[pole1_idx], valid_nodes[pole2_idx]),
            axis_length,
        })
    }

    fn euclidean_distance_sq(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b.iter()).map(|(x, y)| (x - y).powi(2)).sum()
    }

    fn dot_product(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_tesseract_distinct_clusters() {
        let db = AletheiaDB::new().unwrap();

        let mut nodes = Vec::new();

        db.write(|tx| {
            // Cluster A (near 0,0)
            nodes.push(
                tx.create_node(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[0.0, 0.0])
                        .build(),
                )
                .unwrap(),
            );
            nodes.push(
                tx.create_node(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[0.1, 0.0])
                        .build(),
                )
                .unwrap(),
            );
            nodes.push(
                tx.create_node(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[0.0, 0.1])
                        .build(),
                )
                .unwrap(),
            );
            nodes.push(
                tx.create_node(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[-0.1, 0.0])
                        .build(),
                )
                .unwrap(),
            );

            // Cluster B (near 10,0) - clear boundary on X axis
            nodes.push(
                tx.create_node(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[10.0, 0.0])
                        .build(),
                )
                .unwrap(),
            );
            nodes.push(
                tx.create_node(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[10.1, 0.0])
                        .build(),
                )
                .unwrap(),
            );
            nodes.push(
                tx.create_node(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[10.0, 0.1])
                        .build(),
                )
                .unwrap(),
            );
            nodes.push(
                tx.create_node(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[9.9, 0.0])
                        .build(),
                )
                .unwrap(),
            );

            Ok::<(), Error>(())
        })
        .unwrap();

        let tesseract = Tesseract::new(&db);
        let score = tesseract.analyze_boundary(&nodes, "vec").unwrap();

        println!("Distinct Clusters Score: {:?}", score);

        // Huge gap between cluster A (~0.1 max) and B (~9.9 min)
        // Average internal gap is small (~0.1)
        // Score should be very high.
        assert!(
            score.has_boundary(),
            "Should detect a clear boundary between the two clusters"
        );
        assert!(score.boundary_score > 10.0);
    }

    #[test]
    fn test_tesseract_continuous_gradient() {
        let db = AletheiaDB::new().unwrap();

        let mut nodes = Vec::new();

        db.write(|tx| {
            // A smooth line of points from 0 to 10
            for i in 0..=10 {
                let x = i as f32;
                nodes.push(
                    tx.create_node(
                        "Node",
                        PropertyMapBuilder::new()
                            .insert_vector("vec", &[x, 0.0])
                            .build(),
                    )
                    .unwrap(),
                );
            }
            Ok::<(), Error>(())
        })
        .unwrap();

        let tesseract = Tesseract::new(&db);
        let score = tesseract.analyze_boundary(&nodes, "vec").unwrap();

        println!("Continuous Gradient Score: {:?}", score);

        // Points are evenly spaced. Gaps are all exactly 1.0.
        // Max gap = 1.0, Avg gap = 1.0 -> Ratio ~ 1.0
        assert!(
            !score.has_boundary(),
            "Continuous gradient should not be flagged as a boundary"
        );
        assert!(score.boundary_score < 1.5);
    }
}
