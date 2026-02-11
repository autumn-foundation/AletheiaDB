//! Sentinel: The Semantic Firewall 🛡️
//!
//! A semantic validation layer that inspects incoming data (PropertyMaps)
//! and enforces rules based on vector similarity, logic, or other properties.
//!
//! # The Hook
//! "Your database doesn't just store data; it understands it. Sentinel blocks toxic content,
//! off-topic inserts, or logical contradictions *before* they enter the system."
//!
//! # Example
//!
//! ```rust,no_run
//! use aletheiadb::experimental::sentinel::{Sentinel, VectorBanRule, SemanticRule};
//! use aletheiadb::core::property::PropertyMapBuilder;
//!
//! let mut sentinel = Sentinel::new();
//!
//! // Rule: Ban anything semantically similar to "Hate Speech" (vector: [1.0, 0.0])
//! let mut ban_rule = VectorBanRule::new("embedding", 0.9);
//! ban_rule.add_banned_vector(vec![1.0, 0.0]);
//! sentinel.add_rule(Box::new(ban_rule));
//!
//! // This should fail validation
//! let toxic_props = PropertyMapBuilder::new()
//!     .insert_vector("embedding", &[0.95, 0.1])
//!     .build();
//!
//! assert!(sentinel.validate(&toxic_props).is_err());
//! ```

use crate::core::property::PropertyMap;
use crate::utils::error::{Error, Result};

/// A rule that validates a PropertyMap.
pub trait SemanticRule {
    /// Validate the given properties.
    /// Returns Ok if valid, Err with a reason if invalid.
    fn validate(&self, props: &PropertyMap) -> Result<()>;
}

/// The Sentinel validates data against a set of rules.
pub struct Sentinel {
    rules: Vec<Box<dyn SemanticRule>>,
}

impl Sentinel {
    /// Create a new Sentinel with no rules.
    pub fn new() -> Self {
        Self { rules: Vec::new() }
    }

    /// Add a rule to the Sentinel.
    pub fn add_rule(&mut self, rule: Box<dyn SemanticRule>) {
        self.rules.push(rule);
    }

    /// Validate a PropertyMap against all rules.
    pub fn validate(&self, props: &PropertyMap) -> Result<()> {
        for rule in &self.rules {
            rule.validate(props)?;
        }
        Ok(())
    }
}

impl Default for Sentinel {
    fn default() -> Self {
        Self::new()
    }
}

/// A rule that bans vectors similar to a set of forbidden vectors.
pub struct VectorBanRule {
    /// The name of the vector property to check.
    pub property_name: String,
    /// The similarity threshold (0.0 to 1.0).
    /// If similarity > threshold, the validation fails.
    pub threshold: f32,
    /// List of banned vectors.
    banned_vectors: Vec<Vec<f32>>,
}

impl VectorBanRule {
    /// Create a new VectorBanRule.
    pub fn new(property_name: impl Into<String>, threshold: f32) -> Self {
        Self {
            property_name: property_name.into(),
            threshold,
            banned_vectors: Vec::new(),
        }
    }

    /// Add a banned vector to the rule.
    pub fn add_banned_vector(&mut self, vector: Vec<f32>) {
        self.banned_vectors.push(vector);
    }

    /// Calculate cosine similarity between two vectors.
    fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
        if a.len() != b.len() {
            return 0.0;
        }

        let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
        let mag_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let mag_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();

        if mag_a == 0.0 || mag_b == 0.0 {
            0.0
        } else {
            dot / (mag_a * mag_b)
        }
    }
}

impl SemanticRule for VectorBanRule {
    fn validate(&self, props: &PropertyMap) -> Result<()> {
        // If the property doesn't exist or isn't a vector, we skip validation (or should we fail?)
        // Let's be lenient: if no vector is provided, the rule doesn't apply.
        // Unless it's a required field, which is a different rule (SchemaRule).
        let val = match props.get(&self.property_name) {
            Some(v) => v,
            None => return Ok(()),
        };

        let vec = match val.as_vector() {
            Some(v) => v,
            None => return Ok(()), // Not a vector, ignore
        };

        for banned in &self.banned_vectors {
            if vec.len() != banned.len() {
                // Dimension mismatch - ignore or error?
                // Let's ignore for now, assuming different spaces.
                continue;
            }

            let similarity = Self::cosine_similarity(vec, banned);
            if similarity > self.threshold {
                return Err(Error::other(format!(
                    "Vector property '{}' is too similar to a banned vector (similarity: {:.4} > {:.4})",
                    self.property_name, similarity, self.threshold
                )));
            }
        }

        Ok(())
    }
}

/// A rule that enforces a logical condition on a numeric property.
/// e.g. "age >= 18"
pub struct NumericRangeRule {
    /// The property to check.
    pub property_name: String,
    /// Minimum allowed value (inclusive).
    pub min: Option<f64>,
    /// Maximum allowed value (inclusive).
    pub max: Option<f64>,
}

impl NumericRangeRule {
    /// Create a new NumericRangeRule for the given property.
    pub fn new(property_name: impl Into<String>) -> Self {
        Self {
            property_name: property_name.into(),
            min: None,
            max: None,
        }
    }

    /// Set the minimum allowed value.
    pub fn min(mut self, min: f64) -> Self {
        self.min = Some(min);
        self
    }

    /// Set the maximum allowed value.
    pub fn max(mut self, max: f64) -> Self {
        self.max = Some(max);
        self
    }
}

impl SemanticRule for NumericRangeRule {
    fn validate(&self, props: &PropertyMap) -> Result<()> {
        let val = match props.get(&self.property_name) {
            Some(v) => v,
            None => return Ok(()),
        };

        // Try to get as float, then int
        let num = if let Some(f) = val.as_float() {
            f
        } else if let Some(i) = val.as_int() {
            i as f64
        } else {
            return Ok(()); // Not a number
        };

        if let Some(min) = self.min {
            if num < min {
                return Err(Error::other(format!(
                    "Property '{}' value {} is less than minimum {}",
                    self.property_name, num, min
                )));
            }
        }

        if let Some(max) = self.max {
            if num > max {
                return Err(Error::other(format!(
                    "Property '{}' value {} is greater than maximum {}",
                    self.property_name, num, max
                )));
            }
        }

        Ok(())
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_vector_ban_rule() {
        let mut rule = VectorBanRule::new("embedding", 0.9);
        // Banned: [1.0, 0.0]
        rule.add_banned_vector(vec![1.0, 0.0]);

        // Case 1: Identical vector (Should Fail)
        let props1 = PropertyMapBuilder::new()
            .insert_vector("embedding", &[1.0, 0.0])
            .build();
        assert!(rule.validate(&props1).is_err());

        // Case 2: Similar vector (0.95 > 0.9) (Should Fail)
        // [0.99, 0.14] -> normalized ~ [0.99, 0.14]
        // Cosine ~ 0.99
        let props2 = PropertyMapBuilder::new()
            .insert_vector("embedding", &[0.99, 0.14])
            .build();
        assert!(rule.validate(&props2).is_err());

        // Case 3: Different vector (Orthogonal) (Should Pass)
        let props3 = PropertyMapBuilder::new()
            .insert_vector("embedding", &[0.0, 1.0])
            .build();
        assert!(rule.validate(&props3).is_ok());
    }

    #[test]
    fn test_sentinel_integration() {
        let mut sentinel = Sentinel::new();

        // Rule 1: Ban toxic vectors
        let mut ban_rule = VectorBanRule::new("embedding", 0.8);
        ban_rule.add_banned_vector(vec![1.0, 0.0]);
        sentinel.add_rule(Box::new(ban_rule));

        // Rule 2: Age must be >= 18
        let range_rule = NumericRangeRule::new("age").min(18.0);
        sentinel.add_rule(Box::new(range_rule));

        // Valid Insert
        let valid = PropertyMapBuilder::new()
            .insert_vector("embedding", &[0.0, 1.0])
            .insert("age", 25)
            .build();
        assert!(sentinel.validate(&valid).is_ok());

        // Invalid: Toxic
        let toxic = PropertyMapBuilder::new()
            .insert_vector("embedding", &[0.9, 0.1]) // High similarity to [1,0]
            .insert("age", 25)
            .build();
        assert!(sentinel.validate(&toxic).is_err());

        // Invalid: Underage
        let underage = PropertyMapBuilder::new()
            .insert_vector("embedding", &[0.0, 1.0])
            .insert("age", 16)
            .build();
        let res = sentinel.validate(&underage);
        assert!(res.is_err());
        assert!(format!("{}", res.unwrap_err()).contains("less than minimum"));
    }
}
