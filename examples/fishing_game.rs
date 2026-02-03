//! Fishing Minigame UI Example
//!
//! This example demonstrates the "Human Interface" philosophy of Mosaic
//! by wrapping the experimental "Fishing" (associative retrieval) feature
//! in a polished TUI using `ratatui`.
//!
//! Run with: `cargo run --example fishing_game --features nova`

use std::error::Error;

#[cfg(feature = "nova")]
mod game {
    use std::{io, time::{Duration, Instant}};
    use ratatui::{
        crossterm::{
            event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
            execute,
            terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
        },
        prelude::*,
        widgets::*,
    };
    use gallifreydb::{
        GallifreyDB, PropertyMapBuilder,
        experimental::fishing::{FishingRod, Bait, FishingTrip},
        index::vector::{HnswConfig, DistanceMetric},
    };

    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        // Create App first to ensure we don't leave terminal in raw mode if DB init fails
        let mut app = App::new()?;

        // Setup terminal
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        // Run App
        let res = run_app(&mut terminal, &mut app);

        // Restore terminal
        disable_raw_mode()?;
        execute!(
            terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture
        )?;
        terminal.show_cursor()?;

        if let Err(err) = res {
            println!("{:?}", err)
        }

        Ok(())
    }

    struct App {
        db: GallifreyDB,
        tension: f64, // 0.0 to 100.0
        is_fishing: bool,
        caught_fish: Vec<String>,
        logs: Vec<String>,
        bobber_offset: f64,
        start_time: Instant,
    }

    impl App {
        fn new() -> Result<Self, Box<dyn std::error::Error>> {
            let db = GallifreyDB::new()?;

            // Initialize DB with "fish"
            let config = HnswConfig::new(2, DistanceMetric::Cosine);
            db.enable_vector_index("embedding", config)?;

            // Create some fish species with vectors
            let species = vec![
                ("Trout", vec![0.9, 0.1]),
                ("Salmon", vec![0.8, 0.2]),
                ("Bass", vec![0.1, 0.9]),
                ("Pike", vec![0.2, 0.8]),
                ("Goldfish", vec![0.5, 0.5]),
            ];

            for (name, vec) in species {
                let props = PropertyMapBuilder::new()
                    .insert("name", name)
                    .insert_vector("embedding", &vec)
                    .build();
                db.create_node("Fish", props)?;
            }

            Ok(Self {
                db,
                tension: 0.0,
                is_fishing: false,
                caught_fish: Vec::new(),
                logs: vec!["Welcome to GallifreyDB Fishing!".to_string(), "Press SPACE to Cast.".to_string()],
                bobber_offset: 0.0,
                start_time: Instant::now(),
            })
        }

        fn cast(&mut self) {
            if !self.is_fishing {
                self.is_fishing = true;
                self.tension = 0.0;
                self.logs.push("Casting line...".to_string());
            } else {
                // Reel in!
                self.reel();
            }
        }

        fn reel(&mut self) {
            self.is_fishing = false;

            // Perform the "Catch" using FishingRod
            let rod = FishingRod::new(&self.db);

            // Simulate random bait (random vector)
            // In a real game, this might be controlled by player skill/location
            // Here we just use a time-based pseudo-random vector
            let ticks = self.start_time.elapsed().as_millis() as f32;
            let x = (ticks.sin() + 1.0) / 2.0;
            let y = (ticks.cos() + 1.0) / 2.0;

            let bait = Bait::Vector {
                vector: vec![x, y],
                property: Some("embedding".to_string()),
            };

            let trip = FishingTrip {
                limit: 1,
                depth: 0, // Direct catch
                vector_weight: 1.0,
                graph_weight: 0.0,
                freshness_weight: 0.0,
                edge_labels: None,
            };

            self.logs.push("Reeling in...".to_string());

            match rod.cast(bait, trip) {
                Ok(catches) => {
                    if let Some(catch) = catches.first() {
                        // Fetch node details
                        if let Ok(node) = self.db.get_node(catch.node_id) {
                             if let Some(name_prop) = node.properties.get("name") {
                                 let name = name_prop.as_str().unwrap_or("Unknown");
                                 let msg = format!("CAUGHT: {} (Score: {:.2})", name, catch.score);
                                 self.caught_fish.push(msg.clone());
                                 self.logs.push(msg);
                             } else {
                                 self.logs.push("Caught unknown fish!".to_string());
                             }
                        }
                    } else {
                         self.logs.push("Nothing bit...".to_string());
                    }
                }
                Err(e) => {
                    self.logs.push(format!("Line snapped! Error: {}", e));
                }
            }
        }

        fn tick(&mut self) {
            if self.is_fishing {
                // Bobber animation
                let elapsed = self.start_time.elapsed().as_secs_f64();
                self.bobber_offset = (elapsed * 2.0).sin();

                // Tension rises
                if self.tension < 100.0 {
                    self.tension += 0.5;
                }
            } else {
                self.tension = 0.0;
                self.bobber_offset = 0.0;
            }
        }
    }

    fn run_app<B: Backend>(terminal: &mut Terminal<B>, app: &mut App) -> io::Result<()> {
        let tick_rate = Duration::from_millis(100);
        let mut last_tick = Instant::now();

        loop {
            terminal.draw(|f| ui(f, app))?;

            let timeout = tick_rate
                .checked_sub(last_tick.elapsed())
                .unwrap_or_else(|| Duration::from_secs(0));

            if crossterm::event::poll(timeout)? {
                if let Event::Key(key) = event::read()? {
                    match key.code {
                        KeyCode::Char('q') => return Ok(()),
                        KeyCode::Char(' ') => app.cast(),
                        _ => {}
                    }
                }
            }

            if last_tick.elapsed() >= tick_rate {
                app.tick();
                last_tick = Instant::now();
            }
        }
    }

    fn ui(f: &mut Frame, app: &App) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // Header
                Constraint::Min(5),    // Water/Bobber
                Constraint::Length(3), // Tension Bar
                Constraint::Length(10), // Logs
            ])
            .split(f.area());

        // 1. Header
        let title = Paragraph::new("🎣 GallifreyDB Fishing Minigame 🎣")
            .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
            .alignment(Alignment::Center)
            .block(Block::default().borders(Borders::ALL));
        f.render_widget(title, chunks[0]);

        // 2. Water / Bobber
        let water_block = Block::default()
            .title("The Lake")
            .borders(Borders::ALL)
            .style(Style::default().bg(Color::Blue));

        let bobber_y = (chunks[1].height as f64 / 2.0) + app.bobber_offset;
        let bobber_y = bobber_y.clamp(1.0, chunks[1].height as f64 - 2.0) as u16;

        // We can't easily draw a sprite at exact coordinates with standard widgets,
        // but we can use a Canvas or just a Paragraph with newlines.
        // For simplicity, let's use a Paragraph with padded newlines.

        let mut lake_view = String::new();
        for i in 0..chunks[1].height {
            if i == bobber_y {
                lake_view.push_str("          🔴\n"); // The bobber
            } else {
                lake_view.push_str("\n");
            }
        }

        let lake = Paragraph::new(lake_view)
            .block(water_block)
            .style(Style::default().fg(Color::Red)); // Bobber color

        f.render_widget(lake, chunks[1]);


        // 3. Tension Bar
        let tension_ratio = app.tension / 100.0;
        let label = format!("{:.0}%", app.tension);
        let gauge = Gauge::default()
            .block(Block::default().title("Line Tension").borders(Borders::ALL))
            .gauge_style(Style::default().fg(if app.tension > 80.0 { Color::Red } else { Color::Green }))
            .ratio(tension_ratio)
            .label(label);
        f.render_widget(gauge, chunks[2]);

        // 4. Logs / Catch
        let logs: Vec<ListItem> = app.logs
            .iter()
            .rev()
            .take(8)
            .map(|m| ListItem::new(Line::from(vec![Span::raw(m)])))
            .collect();

        let log_list = List::new(logs)
            .block(Block::default().title("Log").borders(Borders::ALL))
            .style(Style::default().fg(Color::White));
        f.render_widget(log_list, chunks[3]);
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    #[cfg(feature = "nova")]
    return game::run();

    #[cfg(not(feature = "nova"))]
    {
        println!("This example requires the 'nova' feature to run.");
        println!("Try: cargo run --example fishing_game --features nova");
        Ok(())
    }
}
