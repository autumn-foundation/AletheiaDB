//! Odometer: Measuring Semantic Distance Over Time
//!
//! "How far has this concept traveled?"
//!
//! The Odometer calculates the cumulative semantic distance a node's vector property
//! has moved across all its historical versions.
//!
//! # Concepts
//! - **Semantic Volatility**: A measure of how much a concept has evolved over time.
//!   High volatility indicates a shifting definition or changing preference.
//!
//! # Use Cases
//! - **Tracking User Preferences**: Finding users whose interests have shifted the most.
//! - **Concept Drift Detection**: Identifying embeddings that are drifting significantly.
//!
//! # Example
//! ```rust,no_run
//! // Requires features = ["nova"]
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::odometer::Odometer;
//! use aletheiadb::core::id::NodeId;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! # let node_id = NodeId::new(1).unwrap();
//!
//! let odometer = Odometer::new(&db);
//! let total_distance = odometer.calculate_distance(node_id, "embedding")?;
//!
//! println!("Total Semantic Distance Traveled: {}", total_distance);
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;

/// The Odometer Engine.
pub struct Odometer<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Odometer<'a> {
    /// Create a new Odometer Engine.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Calculate the cumulative semantic distance a node's vector property
    /// has traveled across all its historical versions.
    ///
    /// # Arguments
    /// * `node_id` - The ID of the node to analyze.
    /// * `property_name` - The vector property to measure.
    pub fn calculate_distance(&self, node_id: NodeId, property_name: &str) -> Result<f32> {
        let history = self.db.get_node_history(node_id)?;

        let mut total_distance = 0.0f32;
        let mut previous_vector: Option<Vec<f32>> = None;

        for version in &history.versions {
            #[allow(clippy::collapsible_if)]
            if let Some(prop) = version.properties.get(property_name) {
                if let Some(current_vector) = prop.as_vector() {
                    let current_vector = current_vector.to_vec();

                    if let Some(prev) = &previous_vector {
                        // Calculate Euclidean distance
                        let distance = euclidean_distance(prev, &current_vector)?;
                        total_distance += distance;
                    }

                    previous_vector = Some(current_vector);
                }
            }
        }

        Ok(total_distance)
    }
}

/// Calculate Euclidean distance between two vectors.
fn euclidean_distance(v1: &[f32], v2: &[f32]) -> Result<f32> {
    if v1.len() != v2.len() {
        return Err(Error::other("Vectors must have the same dimensionality"));
    }

    let sum_sq: f32 = v1.iter().zip(v2.iter()).map(|(a, b)| (a - b).powi(2)).sum();

    Ok(sum_sq.sqrt())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_odometer_distance() {
        let db = AletheiaDB::new().unwrap();

        // Version 1: [0.0, 0.0]
        let props1 = PropertyMapBuilder::new()
            .insert_vector("embedding", &[0.0, 0.0])
            .build();
        let node_id = db.create_node("Concept", props1).unwrap();

        // Version 2: [3.0, 4.0] (Distance = 5.0)
        let props2 = PropertyMapBuilder::new()
            .insert_vector("embedding", &[3.0, 4.0])
            .build();
        db.write(|tx| tx.update_node(node_id, props2)).unwrap();

        // Version 3: [6.0, 8.0] (Distance = 5.0)
        let props3 = PropertyMapBuilder::new()
            .insert_vector("embedding", &[6.0, 8.0])
            .build();
        db.write(|tx| tx.update_node(node_id, props3)).unwrap();

        let odometer = Odometer::new(&db);
        let distance = odometer.calculate_distance(node_id, "embedding").unwrap();

        // Total expected distance = 5.0 + 5.0 = 10.0
        assert!((distance - 10.0).abs() < f32::EPSILON);
    }

    #[test]
    fn test_odometer_no_movement() {
        let db = AletheiaDB::new().unwrap();

        let props1 = PropertyMapBuilder::new()
            .insert_vector("embedding", &[1.0, 1.0])
            .build();
        let node_id = db.create_node("Concept", props1).unwrap();

        let odometer = Odometer::new(&db);
        let distance = odometer.calculate_distance(node_id, "embedding").unwrap();

        assert_eq!(distance, 0.0);
    }

    #[test]
    fn test_odometer_missing_property() {
        let db = AletheiaDB::new().unwrap();

        let props1 = PropertyMapBuilder::new().insert("name", "Test").build();
        let node_id = db.create_node("Concept", props1).unwrap();

        let odometer = Odometer::new(&db);
        let distance = odometer.calculate_distance(node_id, "embedding").unwrap();

        assert_eq!(distance, 0.0);
    }
}
