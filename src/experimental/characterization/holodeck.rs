//! Holodeck: Semantic Graph to 3D HTML Exporter.
//!
//! "Step into the graph."
//!
//! Holodeck exports the AletheiaDB knowledge graph into an interactive
//! standalone HTML file utilizing 3d-force-graph.

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::experimental::characterization::starlight::Starlight;

/// The Holodeck Exporter Engine.
pub struct Holodeck<'a> {
    db: &'a AletheiaDB,
}

#[cfg(feature = "semantic-characterization")]
impl<'a> Holodeck<'a> {
    /// Create a new Holodeck exporter.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Exports an ego-graph centered around `start_node` up to `max_depth` hops as a standalone HTML page.
    pub fn export_ego_graph_html(
        &self,
        start_node: NodeId,
        max_depth: usize,
        max_nodes: Option<usize>,
    ) -> Result<String> {
        let starlight = Starlight::new(self.db);
        let json_payload = starlight.export_ego_graph(start_node, max_depth, max_nodes)?;

        // Sanitize JSON output by escaping '<' as '\u003c' to prevent Local File XSS vulnerabilities.
        let safe_json_payload = json_payload.replace("<", "\\u003c");

        let html = format!(
            r#"<!DOCTYPE html>
<html>
<head>
  <style> body {{ margin: 0; }} </style>
  <script src="//unpkg.com/3d-force-graph"></script>
</head>
<body>
  <div id="3d-graph"></div>
  <script>
    const gData = {};

    const Graph = ForceGraph3D()
      (document.getElementById('3d-graph'))
      .graphData(gData)
      .nodeLabel('name')
      .nodeAutoColorBy('label')
      .linkColor(() => 'rgba(255,255,255,0.2)')
      .linkDirectionalArrowLength(3.5)
      .linkDirectionalArrowRelPos(1);
  </script>
</body>
</html>"#,
            safe_json_payload
        );

        Ok(html)
    }
}

#[cfg(all(test, feature = "semantic-characterization"))]
mod tests {
    use super::*;
    use crate::PropertyMapBuilder;
    use crate::WriteOps;

    #[test]
    fn test_holodeck_html_export() {
        let db = AletheiaDB::new().unwrap();

        let mut node_a = NodeId::new(1).unwrap();
        let mut node_b = NodeId::new(2).unwrap();

        db.write(|tx| {
            // Node A: Alice
            let props_a = PropertyMapBuilder::new()
                .insert("name", "Alice <script>alert(1)</script>")
                .build();
            node_a = tx.create_node("Person", props_a).unwrap();

            // Node B: Bob
            let props_b = PropertyMapBuilder::new().insert("name", "Bob").build();
            node_b = tx.create_node("Person", props_b).unwrap();

            // Edge A -> B
            tx.create_edge(node_a, node_b, "KNOWS", Default::default())
                .unwrap();
            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        let holodeck = Holodeck::new(&db);
        let html_str = holodeck.export_ego_graph_html(node_a, 1, None).unwrap();

        // Check for HTML structure
        assert!(html_str.contains("<html>"));
        assert!(html_str.contains("<script src=\"//unpkg.com/3d-force-graph\"></script>"));
        assert!(html_str.contains("ForceGraph3D()"));

        // Check for embedded JSON payload (which comes from Starlight)
        assert!(html_str.contains("Alice"));
        assert!(html_str.contains("Bob"));
        assert!(html_str.contains("KNOWS"));

        // Check XSS escaping
        assert!(!html_str.contains("<script>alert(1)</script>"));
        assert!(html_str.contains("\\u003cscript>alert(1)\\u003c/script>"));
    }
}
