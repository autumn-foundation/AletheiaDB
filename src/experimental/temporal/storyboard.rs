//! Storyboard: Temporal Narrative to Mermaid Timeline Exporter.
//!
//! "Visualize the journey."
//!
//! Storyboard combines the temporal history tracking of AletheiaDB with Mermaid JS
//! to generate visual timelines of a node's evolution. It acts as a bridge between
//! `NarrativeGenerator` (which computes the diffs) and Markdown visualizations.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::temporal::storyboard::Storyboard;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! # let node_id = aletheiadb::core::id::NodeId::new(1).unwrap();
//! let storyboard = Storyboard::new(&db);
//!
//! // Generate a Mermaid timeline of the node's history
//! let timeline = storyboard.export_timeline(node_id)?;
//! println!("{}", timeline);
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
#[cfg(feature = "semantic-temporal")]
use crate::experimental::temporal::temporal_narrative::NarrativeGenerator;
use std::fmt::Write;

/// The Storyboard Generator.
#[cfg(feature = "semantic-temporal")]
pub struct Storyboard<'a> {
    db: &'a AletheiaDB,
}

#[cfg(feature = "semantic-temporal")]
impl<'a> Storyboard<'a> {
    /// Create a new Storyboard generator.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Exports the temporal narrative of a node as a Mermaid timeline chart.
    pub fn export_timeline(&self, node_id: NodeId) -> Result<String> {
        let generator = NarrativeGenerator::new(self.db);
        let events = generator.generate_node_narrative(node_id)?;

        let node = self.db.get_node(node_id)?;
        let mut title = format!("Node {}", node_id.as_u64());
        if let Some(name) = node.get_property("name") {
            let name_str = name.to_string().replace('"', "");
            title = format!("{} ({})", name_str, title);
        }

        let mut output = String::new();
        writeln!(&mut output, "timeline").unwrap();
        writeln!(&mut output, "    title History of {}", title).unwrap();

        if events.is_empty() {
            writeln!(&mut output, "    No events found").unwrap();
            return Ok(output);
        }

        for event in events {
            // Group events by Version Number
            let safe_desc = event.description.replace(':', "-");
            writeln!(
                &mut output,
                "    Version {} : {}",
                event.version_number, safe_desc
            )
            .unwrap();

            for change in event.changes {
                // Mermaid timeline items after the first colon are bullet points
                let safe_change = change.replace(':', "-").replace('"', "'");
                writeln!(&mut output, "              : {}", safe_change).unwrap();
            }
        }

        Ok(output)
    }
}

#[cfg(all(test, feature = "semantic-temporal"))]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_storyboard_timeline() {
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
                .build();
            tx.update_node(node_id, props2)
        }).unwrap();

        // 3. Export Storyboard
        let storyboard = Storyboard::new(&db);
        let timeline = storyboard.export_timeline(node_id).unwrap();

        assert!(timeline.contains("timeline"));
        assert!(timeline.contains("title History of Alice"));
        assert!(timeline.contains("Version 1 :"));
        assert!(timeline.contains("Initial property 'name'"));
        assert!(timeline.contains("Version 2 :"));
        assert!(timeline.contains("Modified property 'age' from '30' to '31'"));
    }
}
