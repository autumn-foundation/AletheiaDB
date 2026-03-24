//! Temporal Narrative Generator.
//!
//! "Tell me the story of this node."
//!
//! The `NarrativeGenerator` is an experimental tool designed to bridge the gap between
//! structured temporal data and human understanding. Instead of presenting a user with a raw list
//! of properties that changed across multiple versions, it synthesizes these changes into a coherent,
//! chronologically ordered sequence of natural language events.
//!
//! # How it Works
//!
//! Under the hood, the generator reconstructs the complete lifespan of an entity by iterating over
//! its historical versions (using `AletheiaDB::get_node_history`). For each consecutive pair of
//! versions, it utilizes `VersionDiff::compute` to accurately identify which properties were added,
//! modified, or removed. These granular differences are then mapped to string representations,
//! creating a human-readable `NarrativeEvent` for each state change.
//!
//! # Use Cases
//!
//! - **Audit Trails**: Presenting complex data changes to non-technical stakeholders or compliance officers.
//! - **Data Provenance**: Understanding not just *what* the current state of a node is, but the *journey* of how it got there.
//! - **LLM Context**: Providing a summarized, natural language history to Large Language Models for better temporal reasoning.
//!
//! # Example
//!
//! ```rust
//! # #[cfg(feature = "nova")]
//! # fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::temporal_narrative::NarrativeGenerator;
//! use aletheiadb::api::transaction::WriteOps;
//! use aletheiadb::core::property::PropertyMapBuilder;
//!
//! let db = AletheiaDB::new()?;
//!
//! // 1. Create a node
//! let props = PropertyMapBuilder::new()
//!     .insert("name", "Alice")
//!     .insert("status", "Active")
//!     .build();
//! let node_id = db.create_node("User", props)?;
//!
//! // 2. Update it later
//! db.write(|tx| {
//!     let updated_props = PropertyMapBuilder::new()
//!         .insert("name", "Alice")
//!         .insert("status", "Inactive")
//!         .build();
//!     tx.update_node(node_id, updated_props)
//! })?;
//!
//! // 3. Generate the story
//! let generator = NarrativeGenerator::new(&db);
//! let story = generator.generate_node_narrative(node_id)?;
//!
//! for event in story {
//!     println!("Version {}: {}", event.version_number, event.description);
//!     for change in event.changes {
//!         println!("  - {}", change);
//!     }
//! }
//! # Ok(())
//! # }
//! # #[cfg(not(feature = "nova"))]
//! # fn main() {}
//! ```

use crate::AletheiaDB;
#[cfg(feature = "nova")]
use crate::core::GLOBAL_INTERNER;
use crate::core::error::Result;
#[cfg(feature = "nova")]
use crate::core::history::{VersionDiff, VersionInfo};
use crate::core::id::NodeId;
#[cfg(feature = "nova")]
use crate::core::interning::InternedString;
#[cfg(feature = "nova")]
use crate::core::temporal::time;

/// A single event in the narrative history of an entity.
#[derive(Debug, Clone)]
pub struct NarrativeEvent {
    /// ISO 8601 timestamp of when the event was recorded (transaction time).
    pub timestamp: String,
    /// Sequential version number.
    pub version_number: u64,
    /// High-level description of what happened.
    pub description: String,
    /// Detailed list of changes (if any).
    pub changes: Vec<String>,
}

/// Generator for creating natural language narratives from temporal history.
#[cfg(feature = "nova")]
pub struct NarrativeGenerator<'a> {
    db: &'a AletheiaDB,
}

#[cfg(not(feature = "nova"))]
/// Generator for creating natural language narratives from temporal history.
#[deprecated(
    note = "NarrativeGenerator requires the 'nova' feature. Add 'features = [\"nova\"]' to your Cargo.toml."
)]
pub struct NarrativeGenerator<'a> {
    _marker: std::marker::PhantomData<&'a AletheiaDB>,
}

#[cfg(feature = "nova")]
impl<'a> NarrativeGenerator<'a> {
    /// Create a new narrative generator.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Helper to resolve interned keys to strings.
    fn resolve_key(key_id: InternedString) -> String {
        GLOBAL_INTERNER
            .resolve_with(key_id, |s| s.to_string())
            .unwrap_or_else(|| "unknown".to_string())
    }

    /// Generate a narrative for a specific node.
    ///
    /// This reconstructs the history of the node and generates a sequence of
    /// human-readable events describing how it evolved over time.
    ///
    /// # Behavior for Empty History
    /// If a node has no history (e.g. invalid ID or deleted), this returns an empty vector
    /// or an error if the node ID is invalid, consistent with `get_node_history`.
    ///
    /// # Property Removal
    /// Properties set to `null` are interpreted as "Removed property".
    ///
    /// Note: Edge narrative generation is planned for a future release.
    pub fn generate_node_narrative(&self, node_id: NodeId) -> Result<Vec<NarrativeEvent>> {
        let history = self.db.get_node_history(node_id)?;
        let mut events = Vec::new();

        let mut prev_version: Option<&VersionInfo> = None;

        for version in &history.versions {
            let timestamp = time::to_iso8601(version.temporal.transaction_time().start());
            let version_number = version.version_number;
            let mut changes = Vec::new();
            let description;

            if let Some(prev) = prev_version {
                // Determine changes between versions
                let diff = VersionDiff::compute(
                    &prev.properties,
                    &version.properties,
                    prev.version_id,
                    version.version_id,
                );

                description = format!("Version {} updated properties.", version_number);

                // Pre-allocate vector to avoid frequent reallocations
                changes.reserve(diff.added.len() + diff.removed.len() + diff.modified.len());

                // Added properties
                for (key_id, val) in diff.added.iter() {
                    let key = Self::resolve_key(*key_id);
                    changes.push(format!("Added property '{}' with value '{}'", key, val));
                }

                // Removed properties (explicitly missing keys)
                for (key_id, val) in diff.removed.iter() {
                    let key = Self::resolve_key(*key_id);
                    changes.push(format!("Removed property '{}' (was '{}')", key, val));
                }

                // Modified properties
                for (key_id, old_val, new_val) in &diff.modified {
                    let key = Self::resolve_key(*key_id);
                    if new_val.is_null() {
                        // Treat setting to null as removal
                        changes.push(format!("Removed property '{}' (was '{}')", key, old_val));
                    } else {
                        changes.push(format!(
                            "Modified property '{}' from '{}' to '{}'",
                            key, old_val, new_val
                        ));
                    }
                }
            } else {
                // First version (Creation)
                description = format!("Node created with label '{}'.", version.label);
                changes.reserve(version.properties.len());
                for (key_id, val) in version.properties.iter() {
                    let key = Self::resolve_key(*key_id);
                    changes.push(format!("Initial property '{}': '{}'", key, val));
                }
            }

            events.push(NarrativeEvent {
                timestamp,
                version_number,
                description,
                changes,
            });

            prev_version = Some(version);
        }

        Ok(events)
    }
}

#[cfg(not(feature = "nova"))]
#[allow(deprecated)]
impl<'a> NarrativeGenerator<'a> {
    /// Create a new narrative generator.
    ///
    /// # Panics
    ///
    /// This method panics if the `nova` feature is not enabled.
    #[allow(unused_variables)]
    #[track_caller]
    pub fn new(db: &'a AletheiaDB) -> Self {
        panic!(
            "NarrativeGenerator requires the 'nova' feature. Add 'features = [\"nova\"]' to your Cargo.toml."
        );
    }

    /// Generate a narrative for a specific node.
    ///
    /// # Panics
    ///
    /// This method panics if the `nova` feature is not enabled.
    #[allow(unused_variables)]
    #[track_caller]
    pub fn generate_node_narrative(&self, node_id: NodeId) -> Result<Vec<NarrativeEvent>> {
        panic!(
            "NarrativeGenerator requires the 'nova' feature. Add 'features = [\"nova\"]' to your Cargo.toml."
        );
    }
}

#[cfg(all(test, feature = "nova"))]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::{PropertyMapBuilder, PropertyValue};

    #[test]
    fn test_node_narrative_generation() {
        let db = AletheiaDB::new().unwrap();

        // 1. Create Node
        let props1 = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .build();
        let node_id = db.create_node("Person", props1).unwrap();

        // 2. Update Node
        db.write(|tx| {
            let props2 = PropertyMapBuilder::new()
                .insert("name", "Alice")
                .insert("age", 31i64)
                .insert("city", "London")
                .build();
            tx.update_node(node_id, props2)
        })
        .unwrap();

        // 3. Generate Narrative
        let generator = NarrativeGenerator::new(&db);
        let narrative = generator.generate_node_narrative(node_id).unwrap();

        assert_eq!(narrative.len(), 2);

        // Verify First Event (Creation)
        let event1 = &narrative[0];
        assert_eq!(event1.version_number, 1);
        assert!(event1.description.contains("Node created"));
        // PropertyValue::String display format is "Alice" (quoted)
        assert!(
            event1
                .changes
                .iter()
                .any(|s| s.contains("Initial property 'name': '\"Alice\"'"))
        );
        assert!(
            event1
                .changes
                .iter()
                .any(|s| s.contains("Initial property 'age': '30'"))
        );

        // Verify Second Event (Update)
        let event2 = &narrative[1];
        assert_eq!(event2.version_number, 2);
        assert!(event2.description.contains("updated properties"));

        // age changed
        assert!(
            event2
                .changes
                .iter()
                .any(|s| s.contains("Modified property 'age' from '30' to '31'"))
        );
        // city added
        assert!(
            event2
                .changes
                .iter()
                .any(|s| s.contains("Added property 'city' with value '\"London\"'"))
        );
    }

    #[test]
    fn test_property_removal_narrative() {
        let db = AletheiaDB::new().unwrap();

        // 1. Create Node with properties
        let props1 = PropertyMapBuilder::new()
            .insert("name", "Bob")
            .insert("temp_data", "delete_me")
            .build();
        let node_id = db.create_node("Person", props1).unwrap();

        // 2. Remove property by setting to Null (Simulated removal)
        db.write(|tx| {
            let props2 = PropertyMapBuilder::new()
                .insert("temp_data", PropertyValue::Null)
                .build();
            tx.update_node(node_id, props2)
        })
        .unwrap();

        // 3. Generate Narrative
        let generator = NarrativeGenerator::new(&db);
        let narrative = generator.generate_node_narrative(node_id).unwrap();

        assert_eq!(narrative.len(), 2);

        let event2 = &narrative[1];
        assert!(
            event2
                .changes
                .iter()
                .any(|s| s.contains("Removed property 'temp_data'")),
            "Expected removal message for temp_data, found: {:?}",
            event2.changes
        );
        assert!(
            event2
                .changes
                .iter()
                .any(|s| s.contains("was '\"delete_me\"'")),
            "Expected original value in removal message"
        );
    }
}

#[cfg(all(test, not(feature = "nova")))]
#[allow(deprecated)]
mod stub_tests {
    use super::*;

    #[test]
    #[should_panic(
        expected = "NarrativeGenerator requires the 'nova' feature. Add 'features = [\"nova\"]' to your Cargo.toml."
    )]
    fn test_stub_panic_on_new() {
        let db = AletheiaDB::new().unwrap();
        // This should panic
        let _ = NarrativeGenerator::new(&db);
    }

    #[test]
    #[should_panic(
        expected = "NarrativeGenerator requires the 'nova' feature. Add 'features = [\"nova\"]' to your Cargo.toml."
    )]
    fn test_stub_panic_on_generate() {
        // Construct a fake NarrativeGenerator to test method panic
        // Safety: NarrativeGenerator is a ZST with PhantomData, so it's valid to transmute from unit or zeroed.
        // We just need it to call the method.
        let generator: NarrativeGenerator<'_> = NarrativeGenerator {
            _marker: std::marker::PhantomData,
        };
        // This should panic
        let _ = generator.generate_node_narrative(NodeId::new(0).unwrap());
    }
}
