use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    text::Span,
    widgets::{Block, Borders, Paragraph, canvas::Canvas},
};
use std::sync::Arc;
use std::{error::Error, io, time::Duration};

#[cfg(feature = "nova")]
use gallifreydb::GallifreyDB;
#[cfg(feature = "nova")]
use gallifreydb::experimental::digital_garden::DigitalGarden;

#[cfg(not(feature = "nova"))]
fn main() {
    println!("This example requires the 'nova' feature.");
    println!("Run with: cargo run --example digital_garden_demo --features nova");
}

#[cfg(feature = "nova")]
fn main() -> Result<(), Box<dyn Error>> {
    // Setup DB
    let db = GallifreyDB::new()?;
    let db = Arc::new(db);
    let mut garden = DigitalGarden::new(db)?;
    garden.seed(20)?; // Start with 20 plants

    // Setup Terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let res = run_app(&mut terminal, &mut garden);

    // Restore Terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        println!("{:?}", err)
    }

    Ok(())
}

#[cfg(feature = "nova")]
fn run_app<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    garden: &mut DigitalGarden,
) -> io::Result<()> {
    let mut paused = false;

    loop {
        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(0), Constraint::Length(3)])
                .split(f.area());

            // Render Garden Map
            // We map 0.0-1.0 to width/height 100.0
            let area = chunks[0];
            let plants = garden.get_plants();

            let canvas = Canvas::default()
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title("The Memetic Garden"),
                )
                .paint(|ctx| {
                    for plant in plants {
                        // Color based on trait
                        let color = if plant.trait_val < 0.33 {
                            Color::Red
                        } else if plant.trait_val < 0.66 {
                            Color::Green
                        } else {
                            Color::Blue
                        };

                        // Represent health by character
                        let symbol = if plant.health > 0.8 {
                            "♣"
                        } else if plant.health > 0.4 {
                            "♧"
                        } else {
                            "."
                        };

                        ctx.print(
                            plant.x as f64 * 100.0,
                            plant.y as f64 * 100.0,
                            Span::styled(symbol, Style::default().fg(color)),
                        );
                    }
                })
                .x_bounds([0.0, 100.0])
                .y_bounds([0.0, 100.0]);

            f.render_widget(canvas, area);

            // Stats
            let status = format!(
                "Population: {} | Paused: {} | Press 'q' to quit, 'space' to pause",
                garden.population(),
                paused
            );
            let stats = Paragraph::new(status).block(Block::default().borders(Borders::ALL));
            f.render_widget(stats, chunks[1]);
        })?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('q') => return Ok(()),
                        KeyCode::Char(' ') => paused = !paused,
                        _ => {}
                    }
                }
            }
        }

        if !paused {
            // Tick simulation
            if let Err(_) = garden.tick() {
                // Ignore errors (like DB write fails) for demo smoothness
            }
        }
    }
}
