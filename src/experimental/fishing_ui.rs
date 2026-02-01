use crossterm::style::{Color, Stylize};

/// Visual component representing the fishing line tension.
pub struct TensionBar {
    /// Current tension value (0.0 to max_tension).
    pub current_tension: f32,
    /// Maximum tension before the line breaks.
    pub max_tension: f32,
    /// Start of the safe tension zone (green zone).
    pub safe_zone_start: f32,
    /// End of the safe tension zone.
    pub safe_zone_end: f32,
}

impl TensionBar {
    /// Create a new tension bar.
    pub fn new(current: f32, max: f32, safe_start: f32, safe_end: f32) -> Self {
        Self {
            current_tension: current,
            max_tension: max,
            safe_zone_start: safe_start,
            safe_zone_end: safe_end,
        }
    }

    /// Render the tension bar as a styled ASCII string.
    ///
    /// # Arguments
    ///
    /// * `width` - Total width of the bar in characters (including brackets).
    pub fn render(&self, width: usize) -> String {
        let mut bar = String::new();
        bar.push('[');

        let total_chars = width.saturating_sub(2);
        let current_pos = ((self.current_tension / self.max_tension).clamp(0.0, 1.0) * total_chars as f32) as usize;
        let safe_start_pos = ((self.safe_zone_start / self.max_tension).clamp(0.0, 1.0) * total_chars as f32) as usize;
        let safe_end_pos = ((self.safe_zone_end / self.max_tension).clamp(0.0, 1.0) * total_chars as f32) as usize;

        for i in 0..total_chars {
            let ch = if i < current_pos { '|' } else { ' ' };

            // Coloring logic
            let styled_char = if i >= safe_start_pos && i <= safe_end_pos {
                // Inside Safe zone
                if i < current_pos {
                    // Tension inside safe zone - Good!
                    format!("{}", ch.with(Color::Green))
                } else {
                    // Empty safe zone marker (use a different char or color)
                    // Let's use '-' for empty safe zone to make it visible
                    let marker = if i < current_pos { ch } else { '-' };
                    format!("{}", marker.with(Color::DarkGreen))
                }
            } else if i < current_pos {
                // Tension outside safe zone
                if i > safe_end_pos {
                    // High tension (danger)
                     format!("{}", ch.with(Color::Red))
                } else {
                    // Low tension (too slack)
                     format!("{}", ch.with(Color::Yellow))
                }
            } else {
                // Empty space outside safe zone
                format!("{}", ch)
            };

            bar.push_str(&styled_char);
        }

        bar.push(']');
        bar
    }
}

/// State of the fishing bobber.
pub enum BobberState {
    /// Bobber is floating calmly.
    Floating,
    /// Bobber is dipping (fish nibbling).
    Dipping,
    /// Fish is hooked!
    Hooked,
}

/// Icon representing the fishing bobber.
pub struct BobberIcon {
    /// Current state of the bobber.
    pub state: BobberState,
}

impl BobberIcon {
    /// Create a new bobber icon.
    pub fn new(state: BobberState) -> Self {
        Self { state }
    }

    /// Render the bobber icon as a styled string.
    pub fn render(&self) -> String {
        match self.state {
            BobberState::Floating => format!("{}", "O".with(Color::Cyan)),
            BobberState::Dipping => format!("{}", "o".with(Color::Blue)),
            BobberState::Hooked => format!("{}", "!".with(Color::Red).bold()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tension_bar_render() {
        let bar = TensionBar::new(50.0, 100.0, 40.0, 60.0);
        let rendered = bar.render(20);
        assert!(rendered.starts_with('['));
        assert!(rendered.ends_with(']'));
        // We can't easily assert colors in string without parsing ANSI codes,
        // but we can check the length (it will be longer due to ANSI codes).
        assert!(rendered.len() > 20);
    }

    #[test]
    fn test_bobber_render() {
        let bobber = BobberIcon::new(BobberState::Hooked);
        let rendered = bobber.render();
        assert!(rendered.contains('!'));
    }
}
