//! Observatory: Starlight to Interactive HTML Exporter.
//!
//! "Gaze at the stars in your browser."
//!
//! Observatory wraps the Starlight JSON exporter and injects it into a
//! self-contained HTML file featuring `3d-force-graph` for immediate
//! interactive visualization.

use super::starlight::Starlight;
use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;

/// The Observatory Interactive HTML Exporter.
pub struct Observatory<'a> {
    starlight: Starlight<'a>,
}

#[cfg(feature = "semantic-characterization")]
impl<'a> Observatory<'a> {
    /// Create a new Observatory exporter.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self {
            starlight: Starlight::new(db),
        }
    }

    /// Exports an ego-graph centered around `start_node` up to `max_depth` hops
    /// as a self-contained interactive HTML string.
    pub fn export_interactive_html(
        &self,
        start_node: NodeId,
        max_depth: usize,
        max_nodes: Option<usize>,
    ) -> Result<String> {
        let json_data = self
            .starlight
            .export_ego_graph(start_node, max_depth, max_nodes)?;

        // Sanitize JSON for safe inline embedding in HTML script block.
        // Specifically replacing `<` with `\u003c` prevents closing script tags
        // and XSS injection inside the properties.
        let safe_json = json_data.replace("<", "\\u003c");

        let html = format!(
            r#"<!DOCTYPE html>
<html>
<head>
    <title>AletheiaDB Observatory</title>
    <style>
        body {{ margin: 0; padding: 0; overflow: hidden; background-color: #000011; font-family: sans-serif; }}
        #graph-container {{ width: 100vw; height: 100vh; }}
        #info {{ position: absolute; top: 10px; left: 10px; color: white; background: rgba(0,0,0,0.5); padding: 10px; border-radius: 5px; }}
    </style>
    <script src="https://unpkg.com/3d-force-graph"></script>
</head>
<body>
    <div id="info">AletheiaDB Observatory: Ego Graph of {} (Depth: {})</div>
    <div id="graph-container"></div>

    <script>
        const graphData = {};

        const elem = document.getElementById('graph-container');
        const Graph = ForceGraph3D()(elem)
            .graphData(graphData)
            .nodeLabel(node => `${{node.label}}: ${{node.name}}`)
            .nodeAutoColorBy('label')
            .linkDirectionalArrowLength(3.5)
            .linkDirectionalArrowRelPos(1)
            .linkLabel(link => link.label)
            .onNodeClick(node => {{
                // Aim at node from outside it
                const distance = 40;
                const distRatio = 1 + distance/Math.hypot(node.x, node.y, node.z);

                Graph.cameraPosition(
                    {{ x: node.x * distRatio, y: node.y * distRatio, z: node.z * distRatio }}, // new position
                    node, // lookAt ({{ x, y, z }})
                    3000  // ms transition duration
                );
            }});
    </script>
</body>
</html>"#,
            start_node.as_u64(),
            max_depth,
            safe_json
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
    fn test_observatory_html_export_and_xss_prevention() {
        let db = AletheiaDB::new().unwrap();

        let mut node_a = NodeId::new(1).unwrap();

        db.write(|tx| {
            // Node A with malicious script tag
            let props_a = PropertyMapBuilder::new()
                .insert("name", "<script>alert('XSS')</script>")
                .build();
            node_a = tx.create_node("Person", props_a).unwrap();
            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        let observatory = Observatory::new(&db);
        let html_str = observatory
            .export_interactive_html(node_a, 1, None)
            .unwrap();

        // 1. Should contain 3d-force-graph CDN
        assert!(html_str.contains("unpkg.com/3d-force-graph"));

        // 2. Should contain a div element for the graph
        assert!(html_str.contains("<div id=\"graph-container\""));

        // 3. Should contain the escaped script tag (crucial for inline JSON inside HTML scripts)
        assert!(html_str.contains("\\u003cscript>alert('XSS')\\u003c/script>"));
        // Ensure literal unescaped tag is absent
        assert!(!html_str.contains("<script>alert('XSS')</script>\""));
    }
}
