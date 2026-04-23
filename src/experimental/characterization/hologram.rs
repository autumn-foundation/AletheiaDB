//! Hologram: Semantic Graph to 3D HTML Exporter.
//!
//! "Hold the graph in your hands."
//!
//! Hologram uses Starlight to export a JSON representation of an ego-graph,
//! then embeds it directly into a standalone HTML file that renders an
//! interactive 3D graph using `3d-force-graph`.

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::experimental::characterization::starlight::Starlight;

/// Builder for configuring Hologram output.
pub struct HologramBuilder<'a> {
    db: &'a AletheiaDB,
    title: String,
    bg_color: String,
}

#[cfg(feature = "semantic-characterization")]
impl<'a> HologramBuilder<'a> {
    /// Creates a new HologramBuilder.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self {
            db,
            title: "AletheiaDB Hologram".to_string(),
            bg_color: "#000000".to_string(),
        }
    }

    /// Sets the title of the HTML page and graph overlay.
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    /// Sets the background color of the HTML page and graph.
    pub fn bg_color(mut self, bg_color: impl Into<String>) -> Self {
        self.bg_color = bg_color.into();
        self
    }

    /// Builds the Hologram exporter.
    pub fn build(self) -> Hologram<'a> {
        Hologram {
            db: self.db,
            title: self.title,
            bg_color: self.bg_color,
        }
    }
}

/// The Hologram Exporter Engine.
pub struct Hologram<'a> {
    db: &'a AletheiaDB,
    title: String,
    bg_color: String,
}

#[cfg(feature = "semantic-characterization")]
impl<'a> Hologram<'a> {
    /// Exports an ego-graph centered around `start_node` up to `max_depth` hops
    /// into a self-contained 3D HTML file.
    pub fn export_ego_graph_html(
        &self,
        start_node: NodeId,
        max_depth: usize,
        max_nodes: Option<usize>,
    ) -> Result<String> {
        let starlight = Starlight::new(self.db);
        let json_data = starlight.export_ego_graph(start_node, max_depth, max_nodes)?;

        let html = format!(
            r#"<!DOCTYPE html>
<html>
<head>
  <title>{title}</title>
  <style>
    body {{ margin: 0; background-color: {bg_color}; }}
    #graph-info {{ position: absolute; top: 10px; left: 10px; color: white; font-family: sans-serif; pointer-events: none; }}
  </style>
  <script src="https://unpkg.com/3d-force-graph"></script>
</head>
<body>
  <div id="graph-info">
    <h2>{title}</h2>
    <p>Interactive Semantic Graph</p>
  </div>
  <div id="3d-graph"></div>
  <script>
    const gData = {json_data};

    const Graph = ForceGraph3D()
      (document.getElementById('3d-graph'))
        .graphData(gData)
        .nodeLabel('name')
        .nodeAutoColorBy('label')
        .linkDirectionalArrowLength(3.5)
        .linkDirectionalArrowRelPos(1)
        .backgroundColor('{bg_color}');
  </script>
</body>
</html>"#,
            title = self.title,
            bg_color = self.bg_color,
            json_data = json_data
        );

        Ok(html)
    }
}

#[cfg(all(test, feature = "semantic-characterization"))]
mod tests {
    use super::*;
    use crate::PropertyMapBuilder;
    use crate::WriteOps;
    use crate::core::PropertyValue;

    #[test]
    fn test_hologram_html_export() {
        let db = AletheiaDB::new().unwrap();

        let mut node_a = NodeId::new(1).unwrap();
        let mut node_b = NodeId::new(2).unwrap();

        db.write(|tx| {
            let props_a = PropertyMapBuilder::new()
                .insert("name", PropertyValue::string("Alice"))
                .build();
            node_a = tx.create_node("Person", props_a).unwrap();

            let props_b = PropertyMapBuilder::new()
                .insert("name", PropertyValue::string("Bob"))
                .build();
            node_b = tx.create_node("Person", props_b).unwrap();

            tx.create_edge(node_a, node_b, "KNOWS", PropertyMapBuilder::new().build())
                .unwrap();
            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        let hologram = HologramBuilder::new(&db)
            .title("Test Graph")
            .bg_color("#112233")
            .build();

        let html = hologram.export_ego_graph_html(node_a, 1, None).unwrap();

        // Assert HTML boilerplate
        assert!(html.contains("<!DOCTYPE html>"));
        assert!(html.contains("ForceGraph3D"));

        // Assert builder configurations
        assert!(html.contains("<title>Test Graph</title>"));
        assert!(html.contains(".backgroundColor('#112233')"));
        assert!(html.contains("background-color: #112233;"));

        // Assert graph data is embedded (from Starlight JSON)
        assert!(html.contains("Alice"));
        assert!(html.contains("Bob"));
        assert!(html.contains("KNOWS"));
    }
}
