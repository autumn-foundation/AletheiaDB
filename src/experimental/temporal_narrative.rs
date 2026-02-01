use crate::GallifreyDB;
use crate::core::id::NodeId;
use crate::core::temporal::time;
use crate::core::GLOBAL_INTERNER;
use crate::core::history::{VersionDiff, VersionInfo};
use crate::utils::error::Result;

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
pub struct NarrativeGenerator<'a> {
    db: &'a GallifreyDB,
}

impl<'a> NarrativeGenerator<'a> {
    /// Create a new narrative generator.
    pub fn new(db: &'a GallifreyDB) -> Self {
        Self { db }
    }

    /// Generate a narrative for a specific node.
    ///
    /// This reconstructs the history of the node and generates a sequence of
    /// human-readable events describing how it evolved over time.
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

                // Added properties
                for (key_id, val) in diff.added.iter() {
                    let key = GLOBAL_INTERNER.resolve(*key_id).map(|s| s.to_string()).unwrap_or_else(|| "unknown".to_string());
                    changes.push(format!("Added property '{}' with value '{}'", key, val));
                }

                // Removed properties
                for (key_id, val) in diff.removed.iter() {
                    let key = GLOBAL_INTERNER.resolve(*key_id).map(|s| s.to_string()).unwrap_or_else(|| "unknown".to_string());
                    changes.push(format!("Removed property '{}' (was '{}')", key, val));
                }

                // Modified properties
                for (key_id, old_val, new_val) in &diff.modified {
                    let key = GLOBAL_INTERNER.resolve(*key_id).map(|s| s.to_string()).unwrap_or_else(|| "unknown".to_string());
                    changes.push(format!("Modified property '{}' from '{}' to '{}'", key, old_val, new_val));
                }

            } else {
                // First version (Creation)
                description = format!("Node created with label '{}'.", version.label);
                for (key_id, val) in version.properties.iter() {
                    let key = GLOBAL_INTERNER.resolve(*key_id).map(|s| s.to_string()).unwrap_or_else(|| "unknown".to_string());
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;
    use crate::api::transaction::WriteOps;

    #[test]
    fn test_node_narrative_generation() {
        let db = GallifreyDB::new().unwrap();

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
        }).unwrap();

        // 3. Generate Narrative
        let generator = NarrativeGenerator::new(&db);
        let narrative = generator.generate_node_narrative(node_id).unwrap();

        assert_eq!(narrative.len(), 2);

        // Verify First Event (Creation)
        let event1 = &narrative[0];
        assert_eq!(event1.version_number, 1);
        assert!(event1.description.contains("Node created"));
        // PropertyValue::String display format is "Alice" (quoted)
        assert!(event1.changes.iter().any(|s| s.contains("Initial property 'name': '\"Alice\"'")));
        assert!(event1.changes.iter().any(|s| s.contains("Initial property 'age': '30'")));

        // Verify Second Event (Update)
        let event2 = &narrative[1];
        assert_eq!(event2.version_number, 2);
        assert!(event2.description.contains("updated properties"));

        // age changed
        assert!(event2.changes.iter().any(|s| s.contains("Modified property 'age' from '30' to '31'")));
        // city added
        assert!(event2.changes.iter().any(|s| s.contains("Added property 'city' with value '\"London\"'")));
    }
}
