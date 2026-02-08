#![allow(clippy::collapsible_if)]
//! Aletheia Explore - TUI Dashboard & Visualizer
//!
//! Run with: cargo run --bin aletheia-explore --features explore

use std::error::Error;
use std::io;
use std::time::{Duration, Instant};

use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::{Line as TextLine, Span},
    widgets::{
        Block, Borders, List, ListItem, Paragraph, Tabs,
        canvas::{Canvas, Line},
    },
};

use aletheiadb::AletheiaDB;
// Ensure we can access experimental modules
use aletheiadb::core::id::NodeId;
#[cfg(feature = "nova")]
use aletheiadb::experimental::kaleidoscope::{LayoutConfig, LayoutEngine};
use std::collections::HashMap;

enum Tab {
    Home,
    Kaleidoscope,
}

struct App {
    db: AletheiaDB,
    tab: Tab,
    // Kaleidoscope state
    #[cfg(feature = "nova")]
    layout_engine: Option<LayoutEngine>,
    #[cfg(feature = "nova")]
    node_names: HashMap<NodeId, String>,
    #[cfg(feature = "nova")]
    edges: Vec<(NodeId, NodeId)>,
    last_tick: Instant,
    message: String,
}

impl App {
    fn new() -> Result<Self, Box<dyn Error>> {
        let db = AletheiaDB::new()?;

        let mut app = Self {
            db,
            tab: Tab::Home,
            #[cfg(feature = "nova")]
            layout_engine: None,
            #[cfg(feature = "nova")]
            node_names: HashMap::new(),
            #[cfg(feature = "nova")]
            edges: Vec::new(),
            last_tick: Instant::now(),
            message: "Welcome to Aletheia Explore! Press 'k' for Kaleidoscope.".to_string(),
        };

        if app.db.node_count() == 0 {
            app.populate_demo_data()?;
        }

        Ok(app)
    }

    fn populate_demo_data(&mut self) -> Result<(), Box<dyn Error>> {
        use aletheiadb::core::property::PropertyMapBuilder;

        // Create a central "Concept"
        let center = self.db.create_node(
            "Concept",
            PropertyMapBuilder::new()
                .insert("name", "Artificial Intelligence")
                .build(),
        )?;

        // Create satellites
        let topics = [
            "Machine Learning",
            "Neural Networks",
            "Robotics",
            "Computer Vision",
            "NLP",
            "Expert Systems",
            "Logic",
            "Planning",
        ];

        for topic in topics {
            let node = self.db.create_node(
                "Topic",
                PropertyMapBuilder::new().insert("name", topic).build(),
            )?;
            self.db.create_edge(
                center,
                node,
                "RELATED_TO",
                PropertyMapBuilder::new().build(),
            )?;

            // Connect some topics to each other
            if topic == "Machine Learning" {
                let nn = self.db.create_node(
                    "Topic",
                    PropertyMapBuilder::new()
                        .insert("name", "Deep Learning")
                        .build(),
                )?;
                self.db
                    .create_edge(node, nn, "SUBFIELD", PropertyMapBuilder::new().build())?;
            }
        }

        Ok(())
    }

    #[cfg(feature = "nova")]
    fn init_kaleidoscope(&mut self) -> Result<(), Box<dyn Error>> {
        let config = LayoutConfig {
            width: 200.0,
            height: 200.0,
            iterations: 1, // We step manually
            repulsion_strength: 500.0,
            spring_strength: 0.05,
            ..LayoutConfig::default()
        };
        let mut engine = LayoutEngine::new(config);
        self.node_names.clear();
        self.edges.clear();

        // Load graph (naive scan for demo)
        for i in 1..50 {
            // NodeId::new returns Result
            if let Ok(id) = NodeId::new(i) {
                if let Ok(node) = self.db.get_node(id) {
                    engine.add_node(id);
                    if let Some(name) = node.properties.get("name").and_then(|v| v.as_str()) {
                        self.node_names.insert(id, name.to_string());
                    } else {
                        self.node_names.insert(id, format!("Node {}", i));
                    }

                    // get_outgoing_edges returns Vec directly (based on compiler error)
                    // Wait, checking src/db/mod.rs would be safer but let's trust the error message "this expression has type Vec<...>"
                    // But if it fails, I'll fix it.
                    let edges = self.db.get_outgoing_edges(id);
                    // If it was Result, it would be Ok(edges). If it is Vec, it is just edges.
                    // The error said: expected `Vec<EdgeId>`, found `Result<_, _>` when I had `if let Ok(edges)`.
                    // So `get_outgoing_edges` returns `Vec<EdgeId>`.

                    for edge_id in edges {
                        if let Ok(edge) = self.db.get_edge(edge_id) {
                            engine.add_edge(id, edge.target);
                            self.edges.push((id, edge.target));
                        }
                    }
                }
            }
        }

        self.layout_engine = Some(engine);
        self.message = "Kaleidoscope initialized. Physics running...".to_string();
        Ok(())
    }

    #[cfg(feature = "nova")]
    fn on_tick(&mut self) {
        if let Some(engine) = &mut self.layout_engine {
            engine.step();
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create app
    let mut app = App::new()?;

    // Run loop
    let tick_rate = Duration::from_millis(50);
    let res = run_app(&mut terminal, &mut app, tick_rate);

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        println!("{:?}", err);
    }

    Ok(())
}

fn run_app<B: Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    tick_rate: Duration,
) -> io::Result<()> {
    loop {
        terminal.draw(|f| ui(f, app))?;

        let timeout = tick_rate
            .checked_sub(app.last_tick.elapsed())
            .unwrap_or_else(|| Duration::from_secs(0));

        if crossterm::event::poll(timeout)? {
            if let Ok(Event::Key(key)) = event::read() {
                match key.code {
                    KeyCode::Char('q') => return Ok(()),
                    KeyCode::Char('h') => app.tab = Tab::Home,
                    KeyCode::Char('k') => {
                        app.tab = Tab::Kaleidoscope;
                        #[cfg(feature = "nova")]
                        if app.layout_engine.is_none() {
                            let _ = app.init_kaleidoscope();
                        }
                    }
                    _ => {}
                }
            }
        }

        if app.last_tick.elapsed() >= tick_rate {
            #[cfg(feature = "nova")]
            app.on_tick();
            app.last_tick = Instant::now();
        }
    }
}

fn ui(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(
            [
                Constraint::Length(3),
                Constraint::Min(0),
                Constraint::Length(3),
            ]
            .as_ref(),
        )
        .split(f.size());

    let titles = vec![
        TextLine::from("Home (h)"),
        TextLine::from("Kaleidoscope (k)"),
        TextLine::from("Quit (q)"),
    ];
    let tabs = Tabs::new(titles)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Aletheia Explore"),
        )
        .select(match app.tab {
            Tab::Home => 0,
            Tab::Kaleidoscope => 1,
        })
        .style(Style::default().fg(Color::Cyan));

    f.render_widget(tabs, chunks[0]);

    match app.tab {
        Tab::Home => draw_home(f, app, chunks[1]),
        Tab::Kaleidoscope => draw_kaleidoscope(f, app, chunks[1]),
    }

    let status = Paragraph::new(format!("Status: {}", app.message))
        .block(Block::default().borders(Borders::ALL).title("Log"))
        .style(Style::default().fg(Color::Gray));
    f.render_widget(status, chunks[2]);
}

fn draw_home(f: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(5), Constraint::Min(0)].as_ref())
        .split(area);

    let stats = vec![
        ListItem::new(format!("Nodes: {}", app.db.node_count())),
        ListItem::new("Edges: ? (Scan required)".to_string()),
        ListItem::new("Mode: Standalone Demo".to_string()),
    ];

    let list = List::new(stats).block(Block::default().borders(Borders::ALL).title("Statistics"));
    f.render_widget(list, chunks[0]);

    let help = Paragraph::new("Use 'k' to switch to Kaleidoscope view.\nThis demo creates a temporary in-memory database with sample data.")
        .block(Block::default().borders(Borders::ALL).title("Help"));
    f.render_widget(help, chunks[1]);
}

#[cfg(not(feature = "nova"))]
fn draw_kaleidoscope(f: &mut Frame, _app: &App, area: Rect) {
    let p = Paragraph::new("Nova feature not enabled.")
        .block(Block::default().borders(Borders::ALL).title("Error"));
    f.render_widget(p, area);
}

#[cfg(feature = "nova")]
fn draw_kaleidoscope(f: &mut Frame, app: &App, area: Rect) {
    let canvas = Canvas::default()
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Force-Directed Layout"),
        )
        .x_bounds([-500.0, 500.0])
        .y_bounds([-500.0, 500.0])
        .paint(|ctx| {
            if let Some(engine) = &app.layout_engine {
                let positions = engine.get_positions();

                // Draw edges
                for (u, v) in &app.edges {
                    if let (Some(pos_u), Some(pos_v)) = (positions.get(u), positions.get(v)) {
                        ctx.draw(&Line {
                            x1: pos_u.x as f64,
                            y1: pos_u.y as f64,
                            x2: pos_v.x as f64,
                            y2: pos_v.y as f64,
                            color: Color::DarkGray,
                        });
                    }
                }

                // Draw nodes
                for (id, pos) in positions {
                    let color = if id.as_u64() == 1 {
                        Color::Red
                    } else {
                        Color::Yellow
                    };

                    ctx.print(
                        pos.x as f64,
                        pos.y as f64,
                        Span::styled("O", Style::default().fg(color)),
                    );
                    if let Some(name) = app.node_names.get(id) {
                        // Clone the name to own it, avoiding lifetime issues
                        ctx.print(
                            pos.x as f64 + 5.0,
                            pos.y as f64,
                            Span::styled(name.clone(), Style::default().fg(Color::White)),
                        );
                    }
                }
            }
        });
    f.render_widget(canvas, area);
}
