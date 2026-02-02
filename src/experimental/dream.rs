//! Dream Mode: Psychedelic Vector Space Explorer
//!
//! This module implements a "dreaming" process where a vector probe drifts through
//! the high-dimensional semantic space of the database, visualizing the
//! nodes it encounters.

use crate::GallifreyDB;
use crate::utils::Result;
use std::collections::VecDeque;
use rand::Rng;

// TUI Imports
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, List, ListItem, Paragraph, Sparkline},
    Terminal,
};

/// Configuration for the dream.
#[derive(Debug, Clone)]
pub struct DreamConfig {
    /// Dimension of the vector space.
    pub dimensions: usize,
    /// How much random noise to add per tick.
    pub noise_scale: f32,
    /// How much the found results attract the probe (Gravity).
    pub attraction_weight: f32,
    /// Decay of velocity (Friction).
    pub friction: f32,
    /// Max history to keep.
    pub history_limit: usize,
}

impl Default for DreamConfig {
    fn default() -> Self {
        Self {
            dimensions: 1536, // Default OpenAI size
            noise_scale: 0.05,
            attraction_weight: 0.1,
            friction: 0.9,
            history_limit: 100,
        }
    }
}

/// The state of the dream.
pub struct DreamState {
    pub current_vector: Vec<f32>,
    pub velocity: Vec<f32>,
    pub memory_stream: VecDeque<String>,
    pub last_results: Vec<(String, f32)>, // (Label, Score)
    pub arousal: f32, // 0.0 to 1.0
}

/// The engine that drives the dream.
pub struct DreamEngine<'a> {
    db: &'a GallifreyDB,
    config: DreamConfig,
    state: DreamState,
}

impl<'a> DreamEngine<'a> {
    pub fn new(db: &'a GallifreyDB, config: DreamConfig) -> Self {
        let current_vector = vec![0.0; config.dimensions];
        let velocity = vec![0.0; config.dimensions];

        Self {
            db,
            config,
            state: DreamState {
                current_vector,
                velocity,
                memory_stream: VecDeque::new(),
                last_results: Vec::new(),
                arousal: 0.5,
            },
        }
    }

    /// Seed the dream with an initial vector.
    pub fn seed(&mut self, vector: Vec<f32>) {
        if vector.len() == self.config.dimensions {
            self.state.current_vector = vector;
        }
    }

    /// Advance the dream by one tick.
    pub fn tick(&mut self) -> Result<()> {
        let mut rng = rand::thread_rng();

        // 1. Apply noise to velocity
        for i in 0..self.config.dimensions {
            let noise = rng.gen_range(-1.0..1.0) * self.config.noise_scale;
            self.state.velocity[i] += noise;
        }

        // 2. Apply velocity to position
        for i in 0..self.config.dimensions {
            self.state.current_vector[i] += self.state.velocity[i];
        }

        // 3. Normalize position (Keep it on the hypersphere)
        let magnitude: f32 = self.state.current_vector.iter().map(|x| x * x).sum::<f32>().sqrt();
        if magnitude > 1e-6 {
             for x in &mut self.state.current_vector {
                 *x /= magnitude;
             }
        } else {
            // Re-seed if collapsed to zero
            for i in 0..self.config.dimensions {
                self.state.current_vector[i] = rng.gen_range(-1.0..1.0);
            }
            // Normalize again
             let magnitude: f32 = self.state.current_vector.iter().map(|x| x * x).sum::<f32>().sqrt();
             if magnitude > 1e-6 {
                for x in &mut self.state.current_vector {
                    *x /= magnitude;
                }
             }
        }

        // 4. Apply friction to velocity
        for x in &mut self.state.velocity {
            *x *= self.config.friction;
        }

        // 5. Query DB (Visual Cortex)
        // Find index name - prioritize "embedding" or first available
        let indexes = self.db.list_vector_indexes();
        // Just pick the first one for now
        if let Some(idx_info) = indexes.first() {
             let results = self.db.search_vectors_in(&idx_info.property_name, &self.state.current_vector, 5)?;

             // Update last results
             self.state.last_results.clear();

             // In the future: Calculate attraction (Gravity towards memories)
             // For now just record results
             for (node_id, score) in results {
                 // Fetch node label
                 let label = if let Ok(_node) = self.db.get_node(node_id) {
                     // Try to find a name property
                     // But properties are interned. For now just use ID.
                     // TODO: Resolve node labels properly
                     format!("Node {}", node_id.as_u64())
                 } else {
                     format!("Node {}", node_id.as_u64())
                 };

                 self.state.last_results.push((label.clone(), score));

                 // Add to memory stream
                 self.state.memory_stream.push_front(format!("[{:.3}] {}", score, label));
                 if self.state.memory_stream.len() > self.config.history_limit {
                     self.state.memory_stream.pop_back();
                 }
             }
        }

        Ok(())
    }

    /// Get the current vector position.
    pub fn current_position(&self) -> &[f32] {
        &self.state.current_vector
    }

    /// Get the current memory stream (snapshot).
    pub fn memory_stream(&self) -> &VecDeque<String> {
        &self.state.memory_stream
    }

    /// Run the TUI visualization loop.
    pub fn run_tui(&mut self) -> Result<()> {
        // Setup terminal
        enable_raw_mode().map_err(|e| crate::utils::Error::Io(e))?;
        let mut stdout = std::io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture).map_err(|e| crate::utils::Error::Io(e))?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend).map_err(|e| crate::utils::Error::Io(e))?;

        let res = self.run_app(&mut terminal);

        // Restore terminal
        disable_raw_mode().map_err(|e| crate::utils::Error::Io(e))?;
        execute!(
            terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture
        ).map_err(|e| crate::utils::Error::Io(e))?;
        terminal.show_cursor().map_err(|e| crate::utils::Error::Io(e))?;

        if let Err(err) = res {
            println!("{:?}", err);
        }

        Ok(())
    }

    fn run_app<B: Backend>(&mut self, terminal: &mut Terminal<B>) -> std::io::Result<()> {
        loop {
            terminal.draw(|f| {
                let size = f.size();
                let chunks = Layout::default()
                    .direction(Direction::Horizontal)
                    .margin(1)
                    .constraints(
                        [
                            Constraint::Percentage(50),
                            Constraint::Percentage(50),
                        ]
                        .as_ref(),
                    )
                    .split(size);

                // Left Panel: Memory Stream
                let items: Vec<ListItem> = self
                    .state
                    .memory_stream
                    .iter()
                    .map(|m| ListItem::new(m.as_str()))
                    .collect();

                let list = List::new(items)
                    .block(Block::default().borders(Borders::ALL).title("The Cortical Stack"))
                    .highlight_style(Style::default().add_modifier(Modifier::BOLD));

                f.render_widget(list, chunks[0]);

                // Right Panel: Vector State & Status
                let right_chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Percentage(80), Constraint::Percentage(20)].as_ref())
                    .split(chunks[1]);

                // Vector Viz (Sparkline of first 40 dims)
                let spark_data: Vec<u64> = self.state.current_vector.iter().take(40)
                    .map(|&v| ((v + 1.0) * 50.0).clamp(0.0, 100.0) as u64)
                    .collect();

                let sparkline = Sparkline::default()
                    .block(Block::default().title("Neural Activity").borders(Borders::ALL))
                    .data(&spark_data)
                    .style(Style::default().fg(Color::Cyan));

                f.render_widget(sparkline, right_chunks[0]);

                // Status
                let velocity_mag = self.state.velocity.iter().map(|x| x*x).sum::<f32>().sqrt();
                let status_text = format!(
                    "Arousal: {:.2}\nVelocity: {:.4}\n\nPress 'q' to wake up.",
                    self.state.arousal,
                    velocity_mag
                );
                let paragraph = Paragraph::new(status_text)
                     .block(Block::default().borders(Borders::ALL).title("Status"));
                f.render_widget(paragraph, right_chunks[1]);

            })?;

            // Input
            if event::poll(std::time::Duration::from_millis(50))? {
                if let Event::Key(key) = event::read()? {
                    if let KeyCode::Char('q') = key.code {
                        return Ok(());
                    }
                }
            }

            // Tick
            if let Err(_) = self.tick() {
                // Ignore tick errors (like empty DB)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Unused imports were warned about previously, keeping what's needed
    // use crate::HnswConfig;
    // use crate::index::vector::DistanceMetric;
    // use crate::PropertyMapBuilder;

    #[test]
    fn test_dream_cycle() {
        let db = GallifreyDB::new().unwrap();
        // Setup simple 2D space
        let config = DreamConfig {
            dimensions: 2,
            noise_scale: 0.1,
            attraction_weight: 0.1,
            friction: 0.9,
            history_limit: 10,
        };

        let mut engine = DreamEngine::new(&db, config);

        // Seed at origin (or near it, normalized)
        engine.seed(vec![1.0, 0.0]);
        let start_pos = engine.current_position().to_vec();

        // Tick
        engine.tick().unwrap();

        let new_pos = engine.current_position();

        // Should have moved due to noise
        assert_ne!(start_pos, new_pos, "Vector should drift after tick");

        // Should remain normalized
        let magnitude: f32 = new_pos.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((magnitude - 1.0).abs() < 1e-4, "Vector should remain normalized");
    }
}
