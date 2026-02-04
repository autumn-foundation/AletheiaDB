//! Fishing Minigame UI
//!
//! A TUI implementation of the Fishing Minigame using Ratatui.
//! Features:
//! - Tension Bar (Gauge)
//! - Bobber Icon
//! - Fish catching simulation
//!
//! Run with: `cargo run --example fishing_game`

use std::error::Error;
use std::io;
use std::time::{Duration, Instant};

// We use ratatui's re-exported crossterm to ensure compatibility
use ratatui::{
    backend::CrosstermBackend,
    crossterm::{
        event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
        execute,
        terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    },
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Color, Modifier, Style, Stylize},
    text::Span,
    widgets::{Block, Borders, Gauge, Paragraph},
    Terminal,
};

struct App {
    tension: f64,
    fish_position: f64,
    bobber_position: f64,
    score: u32,
    game_over: bool,
    caught: bool,
}

impl App {
    fn new() -> App {
        App {
            tension: 0.0,
            fish_position: 0.5,
            bobber_position: 0.5,
            score: 0,
            game_over: false,
            caught: false,
        }
    }

    fn on_tick(&mut self) {
        if self.game_over || self.caught {
            return;
        }

        // Simulate fish movement (random walk)
        let change = (rand::random::<f64>() - 0.5) * 0.1;
        self.fish_position = (self.fish_position + change).clamp(0.0, 1.0);

        // Tension mechanics
        // If the bobber is far from the fish, tension increases
        let diff = (self.fish_position - self.bobber_position).abs();
        if diff > 0.2 {
            self.tension += 0.02;
        } else {
            self.tension = (self.tension - 0.01).max(0.0);
            self.score += 1;
        }

        // Check fail condition
        if self.tension >= 1.0 {
            self.game_over = true;
        }

        // Check win condition
        if self.score > 200 {
            self.caught = true;
        }
    }

    fn move_bobber(&mut self, delta: f64) {
        if !self.game_over && !self.caught {
            self.bobber_position = (self.bobber_position + delta).clamp(0.0, 1.0);
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    // Setup terminal
    // Important: Initialize fallible resources before enabling raw mode
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Now enable raw mode
    enable_raw_mode()?;

    // Create app
    let mut app = App::new();
    let tick_rate = Duration::from_millis(50);
    let mut last_tick = Instant::now();

    loop {
        terminal.draw(|f| ui(f, &app))?;

        let timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or_else(|| Duration::from_secs(0));

        if ratatui::crossterm::event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Left => app.move_bobber(-0.05),
                    KeyCode::Right => app.move_bobber(0.05),
                    _ => {}
                }
            }
        }

        if last_tick.elapsed() >= tick_rate {
            app.on_tick();
            last_tick = Instant::now();
        }
    }

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if app.caught {
        println!("🎣 You caught the fish! Final Score: {}", app.score);
    } else if app.game_over {
        println!("💔 The line snapped! The fish got away.");
    } else {
        println!("👋 Fishing trip cancelled.");
    }

    Ok(())
}

fn ui(frame: &mut ratatui::Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(2)
        .constraints(
            [
                Constraint::Length(3), // Header
                Constraint::Length(3), // Tension Bar
                Constraint::Min(10),   // Fishing Area
                Constraint::Length(3), // Status
            ]
            .as_ref(),
        )
        .split(frame.area());

    // 1. Header
    let title = Paragraph::new("🎣 Arthropod Fishing Minigame 🎣")
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .alignment(Alignment::Center)
        .block(Block::default().borders(Borders::ALL));
    frame.render_widget(title, chunks[0]);

    // 2. Tension Bar (The requested component)
    // Visual Hierarchy: Tension is critical info
    let tension_percent = (app.tension * 100.0).clamp(0.0, 100.0);
    let tension_label = Span::styled(
        format!("{:.0}%", tension_percent),
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    );

    // Feedback Loop: Color changes with tension (Green -> Yellow -> Red)
    let tension_color = if app.tension < 0.5 {
        Color::Green
    } else if app.tension < 0.8 {
        Color::Yellow
    } else {
        Color::Red
    };

    let gauge = Gauge::default()
        .block(Block::default().title("Line Tension").borders(Borders::ALL))
        .gauge_style(Style::default().fg(tension_color))
        .ratio(app.tension.clamp(0.0, 1.0))
        .label(tension_label);
    frame.render_widget(gauge, chunks[1]);

    // 3. Fishing Area (Bobber Visualization)
    let area_block = Block::default()
        .title("Lake (Keep the Bobber near the Fish!)")
        .borders(Borders::ALL);
    let inner_area = area_block.inner(chunks[2]);
    frame.render_widget(area_block, chunks[2]);

    // Draw Fish and Bobber
    // We adjust coords to stay within bounds
    let width = inner_area.width.saturating_sub(4) as f64; // -4 for padding
    let fish_x = (app.fish_position * width) as usize + 2;
    let bobber_x = (app.bobber_position * width) as usize + 2;

    let fish_icon = "🐟";
    let bobber_icon = "🔴"; // The requested Bobber Icon

    let mut lake_view = String::new();
    lake_view.push_str("\n\n");

    // Fish Line
    let mut fish_line = String::with_capacity(inner_area.width as usize);
    for i in 0..inner_area.width as usize {
        if i == fish_x {
            fish_line.push_str(fish_icon);
        } else if i > fish_x {
             // Fish icon is wide, might skip a space? No, unicode handling is tricky.
             // Just push spaces
             fish_line.push(' ');
        } else {
            fish_line.push(' ');
        }
    }
    // Trim to width to avoid wrap
    let display_fish_line: String = fish_line.chars().take(inner_area.width as usize).collect();
    lake_view.push_str(&display_fish_line);
    lake_view.push_str("\n\n\n");

    // Bobber Line
    let mut bobber_line = String::with_capacity(inner_area.width as usize);
    for i in 0..inner_area.width as usize {
        if i == bobber_x {
            bobber_line.push_str(bobber_icon);
        } else {
            bobber_line.push(' ');
        }
    }
    let display_bobber_line: String = bobber_line.chars().take(inner_area.width as usize).collect();
    lake_view.push_str(&display_bobber_line);

    let lake_widget = Paragraph::new(lake_view).block(Block::default());
    frame.render_widget(lake_widget, inner_area);

    // 4. Status
    let status_text = if app.caught {
        "🎉 FISH CAUGHT! Press 'q' to claim reward.".green().bold()
    } else if app.game_over {
        "💔 LINE SNAPPED! Press 'q' to retreat.".red().bold()
    } else {
        format!(
            "Score: {} | Control: <Left>/<Right> | Quit: <q>",
            app.score
        )
        .into()
    };

    let status = Paragraph::new(status_text)
        .block(Block::default().borders(Borders::ALL));
    frame.render_widget(status, chunks[3]);
}
