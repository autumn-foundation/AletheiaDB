//! Pygmalion: Semantic Imputation Engine.
//!
//! "Your data is incomplete. Pygmalion makes it whole."
//!
//! Pygmalion uses the semantic relationships encoded in vector embeddings to
//! impute (fill in) missing property values for nodes. It assumes that semantically
//! similar nodes likely share similar properties.
//!
//! # Use Cases
//! - **Data Cleaning**: Filling missing fields in imported data.
//! - **Cold Start**: Inferring user preferences based on similar users.
//! - **Attribute Prediction**: Predicting "Category" or "Tags" based on text embedding.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::pygmalion::{Pygmalion, ImputationStrategy};
//! use aletheiadb::core::id::NodeId;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let pygmalion = Pygmalion::new(&db);
//! let node_id = NodeId::new(1).unwrap();
//!
//! // Predict missing "age" using weighted average of 5 nearest neighbors
//! if let Some(age) = pygmalion.impute(node_id, "age", ImputationStrategy::WeightedMean, 5)? {
//!     println!("Predicted age: {}", age);
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::{Error, Result, VectorError};
use crate::core::id::NodeId;
use crate::core::property::PropertyValue;

/// Strategy for aggregating neighbor values.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ImputationStrategy {
    /// Copy the value from the single nearest neighbor.
    /// Works for all data types.
    Nearest,
    /// Calculate the weighted mean based on vector similarity.
    /// Works for Numerics (Int, Float) and Vectors.
    /// For other types, returns an error.
    WeightedMean,
    /// Select the most frequent value (weighted by similarity).
    /// Works for all data types (Categorical imputation).
    Mode,
}

/// The Pygmalion Engine.
pub struct Pygmalion<'a> {
    db: &'a AletheiaDB,
    /// Optional vector property override.
    vector_property: Option<String>,
}

impl<'a> Pygmalion<'a> {
    /// Create a new Pygmalion instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self {
            db,
            vector_property: None,
        }
    }

    /// Set the vector property to use for similarity search.
    pub fn with_vector_property(mut self, name: impl Into<String>) -> Self {
        self.vector_property = Some(name.into());
        self
    }

    /// Resolve the vector property name.
    fn get_vector_property(&self) -> Result<String> {
        if let Some(ref name) = self.vector_property {
            return Ok(name.clone());
        }

        let indexes = self.db.list_vector_indexes();
        if let Some(idx_info) = indexes.first() {
            Ok(idx_info.property_name.clone())
        } else {
            Err(Error::Vector(VectorError::IndexError(
                "No vector indexes configured. Specify a property name or enable an index."
                    .to_string(),
            )))
        }
    }

    /// Impute a missing property value for a target node.
    ///
    /// # Arguments
    /// * `target`: The node ID to impute for.
    /// * `property`: The name of the property to predict.
    /// * `strategy`: The aggregation strategy.
    /// * `k`: The number of neighbors to consider (e.g. 5 or 10).
    ///
    /// # Returns
    /// * `Ok(Some(Value))` if a prediction could be made.
    /// * `Ok(None)` if no suitable neighbors were found.
    /// * `Err` if the target node is invalid or vector operations fail.
    pub fn impute(
        &self,
        target: NodeId,
        property: &str,
        strategy: ImputationStrategy,
        k: usize,
    ) -> Result<Option<PropertyValue>> {
        let vec_prop = self.get_vector_property()?;

        // 1. Get Target Vector
        let target_node = self.db.get_node(target)?;
        let target_vec = match target_node
            .properties
            .get(&vec_prop)
            .and_then(|v| v.as_vector())
        {
            Some(v) => v,
            None => return Ok(None), // Cannot impute without vector
        };

        // 2. Search Neighbors
        // We search for K * 2 to have a buffer for filtering nodes that lack the property.
        let search_k = k * 2;
        let neighbors = self.db.search_vectors_in(&vec_prop, target_vec, search_k)?;

        if neighbors.is_empty() {
            return Ok(None);
        }

        // 3. Collect Values from Neighbors
        let mut values = Vec::new();
        let mut weights = Vec::new();

        for (neighbor_id, similarity) in neighbors {
            if neighbor_id == target {
                continue; // Skip self
            }
            let neighbor = self.db.get_node(neighbor_id)?;
            if let Some(val) = neighbor.properties.get(property) {
                values.push(val.clone());
                // Ensure non-negative weight for arithmetic
                weights.push(similarity.max(0.0));
            }
            if values.len() >= k {
                break;
            }
        }

        if values.is_empty() {
            return Ok(None);
        }

        // 4. Aggregate
        match strategy {
            ImputationStrategy::Nearest => {
                // Neighbors are sorted by similarity descending, so first one is nearest.
                Ok(Some(values[0].clone()))
            }
            ImputationStrategy::WeightedMean => self.aggregate_mean(&values, &weights),
            ImputationStrategy::Mode => self.aggregate_mode(&values, &weights),
        }
    }

    fn aggregate_mean(
        &self,
        values: &[PropertyValue],
        weights: &[f32],
    ) -> Result<Option<PropertyValue>> {
        // Check type of first value
        match values[0] {
            PropertyValue::Int(_) | PropertyValue::Float(_) => {
                let mut sum = 0.0;
                let mut total_weight = 0.0;

                for (val, &w) in values.iter().zip(weights.iter()) {
                    let num = val
                        .as_float()
                        .or_else(|| val.as_int().map(|i| i as f64))
                        .ok_or_else(|| Error::other("Mixed types in weighted mean imputation"))?;
                    sum += num * w as f64;
                    total_weight += w as f64;
                }

                if total_weight == 0.0 {
                    // Fallback to simple mean
                    Ok(Some(PropertyValue::from(sum / values.len() as f64)))
                } else {
                    Ok(Some(PropertyValue::from(sum / total_weight)))
                }
            }
            PropertyValue::Vector(ref v) => {
                let dim = v.len();
                let mut sum_vec = vec![0.0; dim];
                let mut total_weight = 0.0;

                for (val, &w) in values.iter().zip(weights.iter()) {
                    let vec = val.as_vector().ok_or_else(|| {
                        Error::other("Mixed types (expected Vector) in weighted mean imputation")
                    })?;
                    if vec.len() != dim {
                        return Err(Error::Vector(VectorError::DimensionMismatch {
                            expected: dim,
                            actual: vec.len(),
                        }));
                    }
                    for i in 0..dim {
                        sum_vec[i] += vec[i] as f64 * w as f64;
                    }
                    total_weight += w as f64;
                }

                let result: Vec<f32> = if total_weight == 0.0 {
                    let count = values.len() as f64;
                    sum_vec.into_iter().map(|x| (x / count) as f32).collect()
                } else {
                    sum_vec
                        .into_iter()
                        .map(|x| (x / total_weight) as f32)
                        .collect()
                };
                Ok(Some(PropertyValue::from(result)))
            }
            _ => Err(Error::other(
                "WeightedMean strategy only supports Numeric and Vector types",
            )),
        }
    }

    fn aggregate_mode(
        &self,
        values: &[PropertyValue],
        weights: &[f32],
    ) -> Result<Option<PropertyValue>> {
        // We need to hash PropertyValues to count them.
        // PropertyValue doesn't implement Hash, but we can group by string representation or use simple iteration for small K.
        // Or implement a custom aggregation.
        // Since K is small (e.g., 5-20), O(K^2) is fine.

        let mut counts = vec![0.0; values.len()];

        for i in 0..values.len() {
            for j in 0..values.len() {
                if values[i] == values[j] {
                    counts[i] += weights[j];
                }
            }
        }

        // Find max
        let mut max_idx = 0;
        let mut max_score = -1.0;

        for (i, &score) in counts.iter().enumerate() {
            if score > max_score {
                max_score = score;
                max_idx = i;
            }
        }

        Ok(Some(values[max_idx].clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;
    use crate::index::vector::{DistanceMetric, HnswConfig};

    fn setup_db() -> AletheiaDB {
        let db = AletheiaDB::new().unwrap();
        let config = HnswConfig::new(2, DistanceMetric::Cosine);
        db.enable_vector_index("vec", config).unwrap();
        db
    }

    #[test]
    fn test_pygmalion_weighted_mean_numeric() {
        let db = setup_db();
        let pygmalion = Pygmalion::new(&db).with_vector_property("vec");

        // Target: [1.0, 0.0] (Missing "age")
        let target = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[1.0, 0.0])
                    .build(),
            )
            .unwrap();

        // Neighbor 1: [1.0, 0.0] (Sim 1.0), Age 30
        let _n1 = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[1.0, 0.0])
                    .insert("age", 30)
                    .build(),
            )
            .unwrap();

        // Neighbor 2: [0.0, 1.0] (Sim 0.0), Age 50
        // Cosine([1,0], [0,1]) = 0.
        let _n2 = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.0, 1.0])
                    .insert("age", 50)
                    .build(),
            )
            .unwrap();

        // Neighbor 3: [0.707, 0.707] (Sim ~0.707), Age 40
        let _n3 = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.707, 0.707])
                    .insert("age", 40)
                    .build(),
            )
            .unwrap();

        // Weighted Mean:
        // N1: Age 30 * 1.0 = 30
        // N2: Age 50 * 0.0 = 0
        // N3: Age 40 * 0.707 = 28.28
        // Total Weight: 1.707
        // Sum: 58.28
        // Result: 34.14

        let result = pygmalion
            .impute(target, "age", ImputationStrategy::WeightedMean, 5)
            .unwrap()
            .unwrap();
        let age = result.as_float().unwrap();

        assert!(age > 33.0 && age < 35.0, "Expected ~34.14, got {}", age);
    }

    #[test]
    fn test_pygmalion_mode_categorical() {
        let db = setup_db();
        let pygmalion = Pygmalion::new(&db).with_vector_property("vec");

        let target = db
            .create_node(
                "Item",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[1.0, 0.0])
                    .build(),
            )
            .unwrap();

        // N1 (Sim 1.0): "A"
        db.create_node(
            "Item",
            PropertyMapBuilder::new()
                .insert_vector("vec", &[1.0, 0.0])
                .insert("cat", "A")
                .build(),
        )
        .unwrap();

        // N2 (Sim 0.9): "B"
        db.create_node(
            "Item",
            PropertyMapBuilder::new()
                .insert_vector("vec", &[0.9, 0.1]) // Approx
                .insert("cat", "B")
                .build(),
        )
        .unwrap();

        // N3 (Sim 0.8): "A"
        db.create_node(
            "Item",
            PropertyMapBuilder::new()
                .insert_vector("vec", &[0.8, 0.2]) // Approx
                .insert("cat", "A")
                .build(),
        )
        .unwrap();

        // Scores:
        // A: 1.0 + 0.8 = 1.8
        // B: 0.9
        // Winner: A

        let result = pygmalion
            .impute(target, "cat", ImputationStrategy::Mode, 5)
            .unwrap()
            .unwrap();
        assert_eq!(result.as_str(), Some("A"));
    }

    #[test]
    fn test_pygmalion_nearest_neighbor() {
        let db = setup_db();
        let pygmalion = Pygmalion::new(&db).with_vector_property("vec");

        let target = db
            .create_node(
                "Node",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[1.0, 0.0])
                    .build(),
            )
            .unwrap();

        // Closest: 0.99
        db.create_node(
            "Node",
            PropertyMapBuilder::new()
                .insert_vector("vec", &[0.99, 0.05])
                .insert("val", "Near")
                .build(),
        )
        .unwrap();

        // Far: 0.0
        db.create_node(
            "Node",
            PropertyMapBuilder::new()
                .insert_vector("vec", &[0.0, 1.0])
                .insert("val", "Far")
                .build(),
        )
        .unwrap();

        let result = pygmalion
            .impute(target, "val", ImputationStrategy::Nearest, 5)
            .unwrap()
            .unwrap();
        assert_eq!(result.as_str(), Some("Near"));
    }

    #[test]
    fn test_pygmalion_no_neighbors_with_property() {
        let db = setup_db();
        let pygmalion = Pygmalion::new(&db).with_vector_property("vec");

        let target = db
            .create_node(
                "Node",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[1.0, 0.0])
                    .build(),
            )
            .unwrap();

        // Neighbor exists but lacks property
        db.create_node(
            "Node",
            PropertyMapBuilder::new()
                .insert_vector("vec", &[1.0, 0.0])
                .insert("other", "value")
                .build(),
        )
        .unwrap();

        let result = pygmalion
            .impute(target, "missing", ImputationStrategy::Nearest, 5)
            .unwrap();
        assert!(result.is_none());
    }
}
