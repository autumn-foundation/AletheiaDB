#![cfg(feature = "nova")]

//! Dream Engine: The subconscious of GallifreyDB.
//!
//! This module implements a "Dream Mode" where the database autonomously
//! explores its own vector space, "hallucinating" connections and visualizing
//! the semantic topology.

use crate::db::GallifreyDB;
use crate::core::id::NodeId;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use rand::Rng;

/// A single "thought" in the dream, representing an activated node.
#[derive(Debug, Clone)]
pub struct DreamNode {
    /// The ID of the node.
    pub id: NodeId,
    /// The high-dimensional vector.
    pub vector: Vec<f32>,
    /// Activation intensity (0.0 to 1.0).
    pub intensity: f32,
}

/// A "hallucination" is an ephemeral edge created by the dream.
#[derive(Debug, Clone)]
pub struct DreamEdge {
    /// Source node ID.
    pub source: NodeId,
    /// Target node ID.
    pub target: NodeId,
    /// Connection strength.
    pub strength: f32,
}

/// Commands to control the dream.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DreamCommand {
    /// Move forward in the vector space.
    MoveForward,
    /// Move backward.
    MoveBackward,
    /// Drift randomly.
    Drift,
    /// Reset position.
    Reset,
}

/// The Dream Engine orchestrates the dreaming process.
pub struct DreamEngine<'a> {
    db: &'a GallifreyDB,
    dim: usize,
    vector_property: String,
    /// Current position in the vector space (the "dreamer's eye").
    pub position: Vec<f32>,
}

impl<'a> DreamEngine<'a> {
    /// Create a new DreamEngine.
    pub fn new(db: &'a GallifreyDB, dim: usize, vector_property: &str) -> Self {
        let mut rng = rand::thread_rng();
        let position = (0..dim).map(|_| rng.gen_range(-1.0..1.0)).collect();

        Self {
            db,
            dim,
            vector_property: vector_property.to_string(),
            position,
        }
    }

    /// Generate a "Pulse" - a probe into the vector space from current position.
    pub fn pulse(&self) -> Vec<DreamNode> {
        let query = &self.position;
        let mut nodes = Vec::new();

        // Robust Sampling: Use database's sample_nodes API
        if let Ok(sampled_ids) = self.db.sample_nodes(50) {
            for id in sampled_ids {
                 if let Ok(node) = self.db.get_node(id) {
                     if let Some(prop) = node.properties.get(&self.vector_property) {
                         if let Some(vec) = prop.as_vector() {
                             if vec.len() == self.dim {
                                 // Calculate similarity to our current position
                                 let similarity = self.cosine_similarity(query, vec);
                                 nodes.push(DreamNode {
                                     id,
                                     vector: vec.to_vec(),
                                     intensity: similarity,
                                 });
                             }
                         }
                     }
                 }
            }
        }

        // Sort by intensity
        nodes.sort_by(|a, b| b.intensity.partial_cmp(&a.intensity).unwrap_or(std::cmp::Ordering::Equal));
        nodes.truncate(10);
        nodes
    }

    /// Process a user command to navigate the dream.
    pub fn process_command(&mut self, cmd: DreamCommand) {
        let mut rng = rand::thread_rng();
        match cmd {
            DreamCommand::MoveForward => {
                // Move slightly in random direction (brownian motion) or just drift
                // For a "fly through", we ideally need a direction vector.
                // Simplified: slightly perturb position.
                for x in self.position.iter_mut() {
                    *x += rng.gen_range(-0.1..0.1);
                }
            },
            DreamCommand::MoveBackward => {
                for x in self.position.iter_mut() {
                    *x -= rng.gen_range(-0.1..0.1);
                }
            },
            DreamCommand::Drift => {
                // Smaller drift
                for x in self.position.iter_mut() {
                    *x += rng.gen_range(-0.05..0.05);
                }
            },
            DreamCommand::Reset => {
                self.position = (0..self.dim).map(|_| rng.gen_range(-1.0..1.0)).collect();
            }
        }

        // Normalize to keep within reasonable bounds [-1, 1] usually
        // But for "unbounded" dream, we let it drift.
    }

    /// "Hallucinate" connections between activated nodes.
    pub fn hallucinate(&self, nodes: &[DreamNode]) -> Vec<DreamEdge> {
        let mut edges = Vec::new();
        for (i, a) in nodes.iter().enumerate() {
            for b in nodes.iter().skip(i + 1) {
                let dist = self.cosine_similarity(&a.vector, &b.vector);
                // If they are somewhat related, create a dream edge
                if dist > 0.5 {
                     edges.push(DreamEdge {
                         source: a.id,
                         target: b.id,
                         strength: dist,
                     });
                }
            }
        }
        edges
    }

    /// Render a frame of the dream.
    pub fn render_frame(&self, area: Rect, nodes: &[DreamNode], edges: &[DreamEdge]) -> Buffer {
        let mut buffer = Buffer::empty(area);

        // Fill background
        for x in area.left()..area.right() {
            for y in area.top()..area.bottom() {
                 if let Some(cell) = buffer.cell_mut((x, y)) {
                     cell.set_char(' ');
                 }
            }
        }

        // Project nodes to 2D
        // Simple projection: use first two dimensions normalized to screen space
        for node in nodes {
             // Basic projection: map [-1, 1] to screen
             let x_norm = (node.vector[0] + 1.0) / 2.0;
             let y_norm = (node.vector[1] + 1.0) / 2.0;

             let x = (area.left() as f32 + x_norm * (area.width as f32 - 1.0)) as u16;
             let y = (area.top() as f32 + y_norm * (area.height as f32 - 1.0)) as u16;

             if x < area.right() && y < area.bottom() {
                 let symbol = if node.intensity > 0.8 { '*' } else { '.' };
                 let color = if node.intensity > 0.8 { Color::Yellow } else { Color::Blue };
                 if let Some(cell) = buffer.cell_mut((x, y)) {
                     cell.set_char(symbol).set_style(Style::default().fg(color));
                 }
             }
        }

        // Draw edges (Bresenham's line algo or simple interpolation)
        // For simplicity in this moonshot, we just draw the midpoint
        for edge in edges {
            // Find nodes
            if let (Some(src), Some(tgt)) = (
                nodes.iter().find(|n| n.id == edge.source),
                nodes.iter().find(|n| n.id == edge.target)
            ) {
                 let x1 = (src.vector[0] + 1.0) / 2.0 * (area.width as f32 - 1.0);
                 let y1 = (src.vector[1] + 1.0) / 2.0 * (area.height as f32 - 1.0);
                 let x2 = (tgt.vector[0] + 1.0) / 2.0 * (area.width as f32 - 1.0);
                 let y2 = (tgt.vector[1] + 1.0) / 2.0 * (area.height as f32 - 1.0);

                 let mid_x = (area.left() as f32 + (x1 + x2) / 2.0) as u16;
                 let mid_y = (area.top() as f32 + (y1 + y2) / 2.0) as u16;

                 if mid_x < area.right() && mid_y < area.bottom() {
                     if let Some(cell) = buffer.cell_mut((mid_x, mid_y)) {
                         cell.set_char('+').set_style(Style::default().fg(Color::Red));
                     }
                 }
            }
        }

        buffer
    }

    fn cosine_similarity(&self, v1: &[f32], v2: &[f32]) -> f32 {
        let dot: f32 = v1.iter().zip(v2.iter()).map(|(a, b)| a * b).sum();
        let mag1: f32 = v1.iter().map(|x| x * x).sum::<f32>().sqrt();
        let mag2: f32 = v2.iter().map(|x| x * x).sum::<f32>().sqrt();
        if mag1 == 0.0 || mag2 == 0.0 {
            0.0
        } else {
            dot / (mag1 * mag2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_dream_cycle() {
        let db = GallifreyDB::new().unwrap();

        // Seed some memories
        let dim = 3;
        for _ in 0..100 {
            let mut rng = rand::thread_rng();
            let vec: Vec<f32> = (0..dim).map(|_| rng.gen_range(-1.0..1.0)).collect();

            db.create_node(
                "Memory",
                PropertyMapBuilder::new()
                    .insert_vector("thought_vector", &vec)
                    .build()
            ).unwrap();
        }

        let mut engine = DreamEngine::new(&db, dim, "thought_vector");

        // Move around
        engine.process_command(DreamCommand::MoveForward);

        // Pulse
        let active_nodes = engine.pulse();
        println!("Activated nodes: {}", active_nodes.len());

        // Hallucinate
        let hallucinations = engine.hallucinate(&active_nodes);
        println!("Hallucinations: {}", hallucinations.len());

        // Render
        let area = Rect::new(0, 0, 80, 24);
        let buffer = engine.render_frame(area, &active_nodes, &hallucinations);

        // Verify buffer content isn't empty (all spaces)
        let mut has_content = false;
        for x in 0..80 {
            for y in 0..24 {
                if let Some(cell) = buffer.cell((x, y)) {
                    if cell.symbol() != " " {
                        has_content = true;
                        break;
                    }
                }
            }
        }

        // It's possible for a random frame to be empty if no nodes were picked,
        // but statistically unlikely with 100 iterations if logic holds.
        assert!(has_content, "Generated dream frame was empty!");
    }
}
