//! Tapestry: Temporal Timeline Exporter.
//!
//! "Weave the threads of time into a single view."
//!
//! Tapestry translates a node's bi-temporal narrative into a Mermaid.js
//! Timeline diagram. This is ideal for rendering historical audit trails,
//! event streams, and entity evolution in a visually appealing format.

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use std::fmt::Write;

#[cfg(feature = "semantic-temporal")]
use crate::experimental::temporal::temporal_narrative::NarrativeGenerator;

/// The Tapestry Exporter Engine.
pub struct Tapestry<'a> {
    db: &'a AletheiaDB,
}

#[cfg(feature = "semantic-temporal")]
impl<'a> Tapestry<'a> {
    /// Create a new Tapestry exporter.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Exports a chronological Mermaid timeline for the given node's history.
    pub fn export_timeline(&self, node_id: NodeId) -> Result<String> {
        let generator = NarrativeGenerator::new(self.db);
        let narrative = generator.generate_node_narrative(node_id)?;

        let mut output = String::new();
        writeln!(&mut output, "timeline").unwrap();

        let label_str = self
            .db
            .get_node(node_id)
            .map(|n| {
                crate::core::GLOBAL_INTERNER
                    .resolve_with(n.label, |s| s.to_string())
                    .unwrap_or_else(|| "Unknown".to_string())
            })
            .unwrap_or_else(|_| "Unknown".to_string());

        writeln!(&mut output, "    title History of {}", label_str).unwrap();

        for event in narrative {
            // Group by timestamp or version. Mermaid timeline syntax:
            // Section
            //    Time : Event description
            //         : Event detail

            // Mermaid timeline doesn't like very long timestamp strings,
            // so we'll use Version + date part
            let short_time = event
                .timestamp
                .split('T')
                .next()
                .unwrap_or(&event.timestamp);

            writeln!(&mut output, "    section V{}", event.version_number).unwrap();
            writeln!(
                &mut output,
                "        {} : {}",
                short_time, event.description
            )
            .unwrap();

            for change in event.changes {
                writeln!(&mut output, "           : {}", change).unwrap();
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
    fn test_tapestry_mermaid_timeline() {
        let db = AletheiaDB::new().unwrap();

        let props1 = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("status", "Active")
            .build();
        let node_id = db.create_node("Person", props1).unwrap();

        db.write(|tx| {
            let props2 = PropertyMapBuilder::new()
                .insert("name", "Alice")
                .insert("status", "Inactive")
                .build();
            tx.update_node(node_id, props2)
        })
        .unwrap();

        let tapestry = Tapestry::new(&db);
        let chart = tapestry.export_timeline(node_id).unwrap();

        assert!(chart.contains("timeline"));
        assert!(chart.contains("title History of Person"));
        assert!(chart.contains("Modified property 'status' from '\"Active\"' to '\"Inactive\"'"));
        assert!(chart.contains("V1"));
        assert!(chart.contains("V2"));
    }
}
