//! Chronograph: Temporal Narrative to Mermaid Timeline Exporter.
//!
//! "History at a glance."
//!
//! Chronograph takes AletheiaDB's temporal history and outputs a
//! Mermaid JS `timeline` chart. It connects the nodes of events
//! over time, generating an interactive temporal representation.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::characterization::chronograph::Chronograph;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let chronograph = Chronograph::new(&db);
//!
//! # let start_node = aletheiadb::core::id::NodeId::new(1).unwrap();
//! let timeline = chronograph.export_node_timeline(start_node)?;
//! println!("{}", timeline);
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;

#[cfg(feature = "semantic-characterization")]
use crate::experimental::temporal::temporal_narrative::NarrativeGenerator;

/// The Chronograph Exporter Engine.
pub struct Chronograph<'a> {
    db: &'a AletheiaDB,
}

#[cfg(feature = "semantic-characterization")]
impl<'a> Chronograph<'a> {
    /// Create a new Chronograph exporter.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Exports a Node's temporal history as a Mermaid `timeline` string.
    pub fn export_node_timeline(&self, node_id: NodeId) -> Result<String> {
        let generator = NarrativeGenerator::new(self.db);
        let narrative = generator.generate_node_narrative(node_id)?;

        let mut output = String::new();
        output.push_str(
            "timeline
",
        );
        output.push_str(&format!(
            "    title History of Node {}
",
            node_id.as_u64()
        ));

        for event in narrative {
            // Simplified timestamp (just take the date part if ISO, or just as-is)
            let ts = event
                .timestamp
                .split('T')
                .next()
                .unwrap_or(&event.timestamp);

            output.push_str(&format!(
                "    {} : {}
",
                ts, event.description
            ));

            for change in event.changes {
                // Mermaid timeline supports multiple items per time period by indenting
                output.push_str(&format!(
                    "         : {}
",
                    change.replace(":", "-")
                ));
            }
        }

        Ok(output)
    }
}

#[cfg(all(test, feature = "semantic-characterization"))]
mod tests {
    use super::*;
    use crate::PropertyMapBuilder;
    use crate::WriteOps;

    #[test]
    fn test_chronograph_timeline_export() {
        let db = AletheiaDB::new().unwrap();

        // 1. Create Node
        let props1 = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("role", "Engineer")
            .build();
        let node_id = db.create_node("Person", props1).unwrap();

        // 2. Update Node
        db.write(|tx| {
            let props2 = PropertyMapBuilder::new()
                .insert("name", "Alice")
                .insert("role", "Senior Engineer")
                .build();
            tx.update_node(node_id, props2)
        })
        .unwrap();

        let chronograph = Chronograph::new(&db);
        let timeline = chronograph.export_node_timeline(node_id).unwrap();

        assert!(timeline.contains("timeline"));
        assert!(timeline.contains("title History of Node"));
        assert!(timeline.contains("Node created"));
        assert!(timeline.contains("updated properties"));
        assert!(timeline.contains("Senior Engineer"));
    }
}
