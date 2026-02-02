use crate::GallifreyDB;
use crate::core::id::NodeId;
use crate::experimental::temporal_narrative::NarrativeGenerator;
use crate::query::QueryBuilder;

use std::error::Error;
use std::io;

#[cfg(feature = "nova")]
use ratatui::{
    Terminal,
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols,
    text::{Line, Span},
    widgets::{
        Axis, Block, Borders, Chart, Dataset, GraphType, List, ListItem, ListState, Paragraph, Wrap,
    },
};

#[cfg(feature = "nova")]
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};

/// Application state for the Chronoscope TUI.
#[cfg(feature = "nova")]
pub struct ChronoscopeApp<'a> {
    /// Reference to the database.
    db: &'a GallifreyDB,
    /// List of node IDs to display.
    nodes: Vec<NodeId>,
    /// State for the list widget (selected index).
    list_state: ListState,
    /// Narrative generator helper.
    narrative_gen: NarrativeGenerator<'a>,
}

#[cfg(feature = "nova")]
impl<'a> ChronoscopeApp<'a> {
    /// Create a new Chronoscope application.
    pub fn new(db: &'a GallifreyDB) -> Result<Self, Box<dyn Error>> {
        // fetch initial nodes (scan all, limit 100 for now)
        let results = QueryBuilder::new().scan(None).limit(100).execute(db)?;

        let mut nodes = Vec::new();
        for row in results {
            let row = row?;
            if let Some(node) = row.entity.as_node() {
                nodes.push(node.id);
            }
        }

        let mut list_state = ListState::default();
        if !nodes.is_empty() {
            list_state.select(Some(0));
        }

        Ok(Self {
            db,
            nodes,
            list_state,
            narrative_gen: NarrativeGenerator::new(db),
        })
    }

    /// Run the TUI application loop.
    pub fn run(&mut self) -> Result<(), Box<dyn Error>> {
        // Setup terminal
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        // Run loop
        let res = self.run_app(&mut terminal);

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

    fn run_app<B: Backend>(&mut self, terminal: &mut Terminal<B>) -> io::Result<()> {
        loop {
            terminal.draw(|f| self.ui(f))?;

            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') => return Ok(()),
                    KeyCode::Down => self.next(),
                    KeyCode::Up => self.previous(),
                    _ => {}
                }
            }
        }
    }

    fn next(&mut self) {
        if self.nodes.is_empty() {
            return;
        }
        let i = match self.list_state.selected() {
            Some(i) => {
                if i >= self.nodes.len() - 1 {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    fn previous(&mut self) {
        if self.nodes.is_empty() {
            return;
        }
        let i = match self.list_state.selected() {
            Some(i) => {
                if i == 0 {
                    self.nodes.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    fn ui(&mut self, f: &mut ratatui::Frame<'_>) {
        let size = f.area();

        // Layout: Left (List), Right (Details + Chart)
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(30), Constraint::Percentage(70)].as_ref())
            .split(size);

        let right_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)].as_ref())
            .split(chunks[1]);

        self.render_node_list(f, chunks[0]);
        self.render_node_details(f, right_chunks[0]);
        self.render_temporal_chart(f, right_chunks[1]);
    }

    fn render_node_list(&mut self, f: &mut ratatui::Frame<'_>, area: Rect) {
        let items: Vec<ListItem<'_>> = self
            .nodes
            .iter()
            .map(|id| {
                // Try to get label if possible, or just ID
                let label = if let Ok(node) = self.db.get_node(*id) {
                    let label_str = crate::core::GLOBAL_INTERNER
                        .resolve(node.label)
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| format!("{}", node.label));
                    format!("Node {} ({})", id.as_u64(), label_str)
                } else {
                    format!("Node {}", id.as_u64())
                };
                ListItem::new(Line::from(label))
            })
            .collect();

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title("Nodes"))
            .highlight_style(
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .fg(Color::Yellow),
            )
            .highlight_symbol(">> ");

        f.render_stateful_widget(list, area, &mut self.list_state);
    }

    fn render_node_details(&mut self, f: &mut ratatui::Frame<'_>, area: Rect) {
        let selected_id = self.list_state.selected().and_then(|i| self.nodes.get(i));

        let text = if let Some(&id) = selected_id {
            match self.narrative_gen.generate_node_narrative(id) {
                Ok(narrative) => {
                    let mut lines = vec![
                        Line::from(Span::styled(
                            format!("History for Node {}", id.as_u64()),
                            Style::default().add_modifier(Modifier::BOLD),
                        )),
                        Line::from(""),
                    ];

                    for event in narrative {
                        lines.push(Line::from(Span::styled(
                            format!("[{}] {}", event.timestamp, event.description),
                            Style::default().fg(Color::Cyan),
                        )));
                        for change in event.changes {
                            lines.push(Line::from(format!("  - {}", change)));
                        }
                        lines.push(Line::from(""));
                    }
                    lines
                }
                Err(e) => vec![Line::from(format!("Error: {}", e))],
            }
        } else {
            vec![Line::from("Select a node...")]
        };

        let paragraph = Paragraph::new(text)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Narrative Log"),
            )
            .wrap(Wrap { trim: true });

        f.render_widget(paragraph, area);
    }

    fn render_temporal_chart(&mut self, f: &mut ratatui::Frame<'_>, area: Rect) {
        let selected_id = self.list_state.selected().and_then(|i| self.nodes.get(i));

        // Data points: (Valid Time, Transaction Time)
        let mut data = Vec::new();

        let mut x_min = f64::MAX;
        let mut x_max = f64::MIN;
        let mut y_min = f64::MAX;
        let mut y_max = f64::MIN;

        if let Some(history) = selected_id.and_then(|&id| self.db.get_node_history(id).ok()) {
            for version in history.versions {
                // Normalize timestamps to something chartable (e.g. seconds from epoch / 1000)
                // or just raw u64 cast to f64 (precision loss might be an issue for large timestamps but okay for vis)
                let vt = version.temporal.valid_time().start().wallclock() as f64;
                let tt = version.temporal.transaction_time().start().wallclock() as f64;

                data.push((vt, tt));

                if vt < x_min {
                    x_min = vt;
                }
                if vt > x_max {
                    x_max = vt;
                }
                if tt < y_min {
                    y_min = tt;
                }
                if tt > y_max {
                    y_max = tt;
                }
            }
        }

        // Add padding
        let x_padding = if x_max == x_min {
            100.0
        } else {
            (x_max - x_min) * 0.1
        };
        let y_padding = if y_max == y_min {
            100.0
        } else {
            (y_max - y_min) * 0.1
        };

        let datasets = vec![
            Dataset::default()
                .name("Versions")
                .marker(symbols::Marker::Dot)
                .graph_type(GraphType::Scatter)
                .style(Style::default().fg(Color::Magenta))
                .data(&data),
        ];

        let chart = Chart::new(datasets)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Temporal Map (VT vs TT)"),
            )
            .x_axis(
                Axis::default()
                    .title("Valid Time")
                    .style(Style::default().fg(Color::Gray))
                    .bounds([x_min - x_padding, x_max + x_padding])
                    .labels(vec![Span::from("Start"), Span::from("End")]),
            )
            .y_axis(
                Axis::default()
                    .title("Transaction Time")
                    .style(Style::default().fg(Color::Gray))
                    .bounds([y_min - y_padding, y_max + y_padding])
                    .labels(vec![Span::from("Old"), Span::from("New")]),
            );

        f.render_widget(chart, area);
    }
}

#[cfg(all(test, feature = "nova"))]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_chronoscope_app_init() {
        let db = GallifreyDB::new().unwrap();

        // Create some nodes
        let id1 = db
            .write(|tx| {
                tx.create_node(
                    "Person",
                    PropertyMapBuilder::new().insert("name", "Alice").build(),
                )
            })
            .unwrap();

        let id2 = db
            .write(|tx| {
                tx.create_node(
                    "Person",
                    PropertyMapBuilder::new().insert("name", "Bob").build(),
                )
            })
            .unwrap();

        let mut app = ChronoscopeApp::new(&db).unwrap();

        // Check initialization
        assert_eq!(app.nodes.len(), 2);
        assert!(app.nodes.contains(&id1));
        assert!(app.nodes.contains(&id2));
        assert_eq!(app.list_state.selected(), Some(0));

        // Test navigation
        app.next();
        assert_eq!(app.list_state.selected(), Some(1));

        app.next();
        assert_eq!(app.list_state.selected(), Some(0)); // Loop back

        app.previous();
        assert_eq!(app.list_state.selected(), Some(1)); // Loop back
    }
}
