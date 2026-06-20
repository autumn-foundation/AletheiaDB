//! Chronicle: Semantic Graph to Mermaid Timeline Exporter.
//!
//! "History at a glance."
//!
//! Chronicle exports the temporal history of a node into a Mermaid JS `timeline` chart.
//! It uses `NarrativeGenerator` under the hood to reconstruct events, and maps them
//! into a beautiful, linear visual representation.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::characterization::chronicle::Chronicle;
//! use aletheiadb::core::id::NodeId;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let chronicle = Chronicle::new(&db);
//!
//! # let start_node = aletheiadb::core::id::NodeId::new(1).unwrap();
//! let timeline_chart = chronicle.export_timeline(start_node)?;
//! println!("{}", timeline_chart);
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use std::fmt::Write;

#[cfg(feature = "semantic-characterization")]
use crate::experimental::temporal_narrative::NarrativeGenerator;

/// The Chronicle Exporter Engine.
pub struct Chronicle<'a> {
    db: &'a AletheiaDB,
}

#[cfg(feature = "semantic-characterization")]
impl<'a> Chronicle<'a> {
    /// Create a new Chronicle exporter.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Exports the narrative history of `node_id` into a Mermaid Timeline format.
    pub fn export_timeline(&self, node_id: NodeId) -> Result<String> {
        let generator = NarrativeGenerator::new(self.db);
        let events = generator.generate_node_narrative(node_id)?;

        let mut output = String::new();
        writeln!(&mut output, "timeline")
            .map_err(|e| crate::core::error::Error::other(e.to_string()))?;
        writeln!(
            &mut output,
            "    title History of Node {}",
            node_id.as_u64()
        )
        .map_err(|e| crate::core::error::Error::other(e.to_string()))?;

        if events.is_empty() {
            writeln!(&mut output, "    No History : Available")
                .map_err(|e| crate::core::error::Error::other(e.to_string()))?;
            return Ok(output);
        }

        for event in events {
            // Ensure no invalid characters in timestamp for Mermaid timeline sections
            let timestamp_safe = event.timestamp.replace(":", "-").replace("T", " ");

            writeln!(
                &mut output,
                "    {} : Version {}",
                timestamp_safe, event.version_number
            )
            .map_err(|e| crate::core::error::Error::other(e.to_string()))?;

            writeln!(
                &mut output,
                "             : {}",
                Self::escape_mermaid(&event.description)
            )
            .map_err(|e| crate::core::error::Error::other(e.to_string()))?;

            for change in event.changes {
                writeln!(
                    &mut output,
                    "             : {}",
                    Self::escape_mermaid(&change)
                )
                .map_err(|e| crate::core::error::Error::other(e.to_string()))?;
            }
        }

        Ok(output)
    }

    /// Replaces characters that might break mermaid timeline syntax.
    fn escape_mermaid(s: &str) -> String {
        s.replace(":", "-").replace("\n", " ")
    }
}

#[cfg(all(test, feature = "semantic-characterization"))]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_chronicle_timeline_export() {
        let db = AletheiaDB::new().unwrap();

        // Create node
        let props1 = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("role", "Intern")
            .build();
        let node_id = db.create_node("Person", props1).unwrap();

        // Update node
        db.write(|tx| {
            let props2 = PropertyMapBuilder::new()
                .insert("name", "Alice")
                .insert("role", "Engineer")
                .build();
            tx.update_node(node_id, props2)
        })
        .unwrap();

        let chronicle = Chronicle::new(&db);
        let timeline = chronicle.export_timeline(node_id).unwrap();

        assert!(timeline.starts_with("timeline"));
        assert!(timeline.contains(&format!("title History of Node {}", node_id.as_u64())));

        // Verify version 1 info
        assert!(timeline.contains(": Version 1"));
        assert!(timeline.contains(": Node created with label 'Person'."));
        assert!(timeline.contains(": Initial property 'name'- '\"Alice\"'"));
        assert!(timeline.contains(": Initial property 'role'- '\"Intern\"'"));

        // Verify version 2 info
        assert!(timeline.contains(": Version 2"));
        assert!(timeline.contains(": Version 2 updated properties."));
        assert!(
            timeline.contains(": Modified property 'role' from '\"Intern\"' to '\"Engineer\"'")
        );
    }

    #[test]
    fn test_chronicle_escape_mermaid() {
        let text = "This: has\nnewlines";
        let escaped = Chronicle::escape_mermaid(text);
        assert_eq!(escaped, "This- has newlines");
    }
}
