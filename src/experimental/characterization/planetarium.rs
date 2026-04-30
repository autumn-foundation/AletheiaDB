#![cfg(feature = "semantic-characterization")]
//! Planetarium: Self-Contained 3D HTML Web Visualizer.
//!
//! "Hold the universe in your browser."
//!
//! Planetarium takes the graph and generates a fully self-contained HTML file
//! powered by `3d-force-graph` and the `Starlight` JSON exporter.
//! It allows users to instantly visualize their knowledge graph in interactive 3D.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::characterization::planetarium::Planetarium;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let planetarium = Planetarium::new(&db);
//!
//! # let start_node = aletheiadb::core::id::NodeId::new(1).unwrap();
//! let html = planetarium.export_3d_html(start_node, 2, Some(500))?;
//! std::fs::write("graph.html", html).unwrap();
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::experimental::characterization::starlight::Starlight;

/// The Planetarium HTML Exporter Engine.
pub struct Planetarium<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Planetarium<'a> {
    /// Create a new Planetarium exporter.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Exports an ego-graph centered around `start_node` up to `max_depth` hops
    /// as a self-contained HTML document using 3d-force-graph.
    pub fn export_3d_html(
        &self,
        start_node: NodeId,
        max_depth: usize,
        max_nodes: Option<usize>,
    ) -> Result<String> {
        let starlight = Starlight::new(self.db);
        let graph_json = starlight.export_ego_graph(start_node, max_depth, max_nodes)?;

        // Prevent XSS by escaping HTML tags in the JSON
        let safe_json = graph_json.replace("<", "\\u003c");

        let html = format!(
            r#"<!DOCTYPE html>
<html>
<head>
    <title>AletheiaDB Planetarium</title>
    <style> body {{ margin: 0; }} </style>
    <script src="https://unpkg.com/3d-force-graph"></script>
</head>
<body>
    <div id="3d-graph"></div>

    <script>
        const graphData = {};

        const Graph = ForceGraph3D()
        (document.getElementById('3d-graph'))
            .graphData(graphData)
            .nodeLabel('name')
            .nodeAutoColorBy('label')
            .linkDirectionalArrowLength(3.5)
            .linkDirectionalArrowRelPos(1)
            .onNodeClick(node => {{
                const distance = 40;
                const distRatio = 1 + distance/Math.hypot(node.x, node.y, node.z);

                const newPos = node.x || node.y || node.z
                    ? {{ x: node.x * distRatio, y: node.y * distRatio, z: node.z * distRatio }}
                    : {{ x: 0, y: 0, z: distance }}; // special case if node is at (0,0,0)

                Graph.cameraPosition(
                    newPos, // new position
                    node, // lookAt ({{ x, y, z }})
                    3000  // ms transition duration
                );
            }});
    </script>
</body>
</html>"#,
            safe_json
        );

        Ok(html)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;
    use crate::api::transaction::WriteOps;

    #[test]
    fn test_planetarium_html_export() {
        let db = AletheiaDB::new().unwrap();

        let mut node_a = NodeId::new(1).unwrap();

        db.write(|tx| {
            let props_a = PropertyMapBuilder::new().insert("name", "Alice").build();
            node_a = tx.create_node("Person", props_a).unwrap();
            Ok::<(), crate::core::error::Error>(())
        }).unwrap();

        let planetarium = Planetarium::new(&db);
        let html_str = planetarium.export_3d_html(node_a, 1, None).unwrap();

        assert!(html_str.contains("<!DOCTYPE html>"), "Missing HTML boilerplate");
        assert!(html_str.contains("3d-force-graph"), "Missing force graph dependency");
        assert!(html_str.contains("Alice"), "Missing node data in JSON");
    }

    #[test]
    fn test_planetarium_xss_prevention() {
        let db = AletheiaDB::new().unwrap();

        let mut node_a = NodeId::new(1).unwrap();

        db.write(|tx| {
            let props_a = PropertyMapBuilder::new().insert("name", "<script>alert(1)</script>").build();
            node_a = tx.create_node("Person", props_a).unwrap();
            Ok::<(), crate::core::error::Error>(())
        }).unwrap();

        let planetarium = Planetarium::new(&db);
        let html_str = planetarium.export_3d_html(node_a, 1, None).unwrap();

        assert!(!html_str.contains("<script>alert"), "HTML injection vulnerability detected");
        assert!(html_str.contains("\\u003cscript>alert"), "JSON not properly escaped");
    }
}
