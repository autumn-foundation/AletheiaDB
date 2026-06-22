//! Zoetrope: Temporal History to Animation Frames Exporter.
//!
//! "Watch the graph evolve."
//!
//! Zoetrope traverses the temporal history of a node
//! and generates a sequence of "frames" (JSON snapshots) representing
//! its state at each point in time. This is perfect for building
//! animations or sliders in a UI to visualize how a concept evolved.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::characterization::zoetrope::Zoetrope;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let zoetrope = Zoetrope::new(&db);
//!
//! # let node_id = aletheiadb::core::id::NodeId::new(1).unwrap();
//! // Export the history of a specific node as animation frames
//! let frames_json = zoetrope.export_node_history_frames(node_id)?;
//! println!("{}", frames_json);
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use serde_json::json;

/// The Zoetrope Exporter Engine.
pub struct Zoetrope<'a> {
    db: &'a AletheiaDB,
}

#[cfg(feature = "semantic-characterization")]
impl<'a> Zoetrope<'a> {
    /// Create a new Zoetrope exporter.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Exports the temporal history of a node into a JSON array of frames.
    pub fn export_node_history_frames(&self, node_id: NodeId) -> Result<String> {
        let history = self.db.get_node_history(node_id)?;

        let mut frames = Vec::new();

        for version in history.versions {
            let mut props_json = serde_json::Map::new();
            for (k, v) in version.properties.iter() {
                // Simplified property to string conversion for JSON visualization
                props_json.insert(k.to_string(), json!(v.to_string()));
            }

            frames.push(json!({
                "version_number": version.version_number,
                "label": version.label,
                "valid_from": version.temporal.valid_time().start().wallclock(),
                "recorded_at": version.temporal.transaction_time().start().wallclock(),
                "properties": props_json
            }));
        }

        let animation = json!({
            "node_id": node_id.as_u64(),
            "frames": frames,
        });

        serde_json::to_string(&animation)
            .map_err(|e| crate::core::error::Error::other(e.to_string()))
    }
}

#[cfg(all(test, feature = "semantic-characterization"))]
mod tests {
    use super::*;
    use crate::PropertyMapBuilder;
    use crate::WriteOps;

    #[test]
    fn test_zoetrope_json_export() {
        let db = AletheiaDB::new().unwrap();

        let mut node_id = NodeId::new(1).unwrap();

        db.write(|tx| {
            // Version 1
            let props1 = PropertyMapBuilder::new().insert("status", "draft").build();
            node_id = tx.create_node("Document", props1).unwrap();
            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        db.write(|tx| {
            // Version 2
            let props2 = PropertyMapBuilder::new()
                .insert("status", "published")
                .build();
            tx.update_node(node_id, props2).unwrap();
            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        let zoetrope = Zoetrope::new(&db);
        let json_str = zoetrope.export_node_history_frames(node_id).unwrap();

        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();

        assert_eq!(parsed["node_id"], node_id.as_u64());
        let frames = parsed["frames"].as_array().unwrap();
        assert_eq!(frames.len(), 2);

        assert_eq!(frames[0]["version_number"], 1);
        assert_eq!(frames[0]["properties"]["status"], "\"draft\"");

        assert_eq!(frames[1]["version_number"], 2);
        assert_eq!(frames[1]["properties"]["status"], "\"published\"");
    }
}
