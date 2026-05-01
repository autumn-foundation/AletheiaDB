use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::experimental::characterization::starlight::Starlight;

/// The Hologram 3D HTML Exporter.
pub struct Hologram<'a> {
    db: &'a AletheiaDB,
}

#[cfg(feature = "semantic-characterization")]
impl<'a> Hologram<'a> {
    /// Create a new Hologram exporter.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Exports an ego-graph centered around `start_node` up to `max_depth` hops as an interactive 3D HTML document.
    pub fn export_3d_html(
        &self,
        start_node: NodeId,
        max_depth: usize,
        max_nodes: Option<usize>,
    ) -> Result<String> {
        let starlight = Starlight::new(self.db);
        let json_data = starlight.export_ego_graph(start_node, max_depth, max_nodes)?;

        // Sanitize JSON to prevent XSS attacks when interpolated into HTML script tags.
        let sanitized_json = json_data.replace('<', "\\u003c");

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
        .linkDirectionalArrowLength(3.5)
        .linkDirectionalArrowRelPos(1);
  </script>
</body>
</html>"#,
            sanitized_json
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
    fn test_hologram_html_export() {
        let db = AletheiaDB::new().unwrap();

        let mut node_a = NodeId::new(1).unwrap();
        let mut node_b = NodeId::new(2).unwrap();

        db.write(|tx| {
            // Node A: Alice with a malicious property
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

        let hologram = Hologram::new(&db);
        let html_str = hologram.export_3d_html(node_a, 1, None).unwrap();

        // Check for HTML boilerplate
        assert!(html_str.contains("<html>"));
        assert!(html_str.contains("3d-force-graph"));

        // Check for node data
        assert!(html_str.contains("Alice"));
        assert!(html_str.contains("Bob"));

        // Check for sanitization (escaping `<` to prevent XSS)
        assert!(!html_str.contains("<script>alert(1)</script>"));
        assert!(html_str.contains("\\u003cscript>alert(1)\\u003c/script>"));
    }
}
