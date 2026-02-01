use crossterm::style::{Color, Stylize};

/// State of a UI button.
pub enum ButtonState {
    /// Normal state.
    Idle,
    /// Mouse over state.
    Hover,
    /// Active/Pressed state.
    Clicked,
}

/// A reusable button component.
///
/// Refactored from previous implementation to separate state from rendering.
pub struct Button {
    /// Text label on the button.
    pub label: String,
    /// Current interaction state.
    pub state: ButtonState,
}

impl Button {
    /// Create a new button with the given label.
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            state: ButtonState::Idle,
        }
    }

    /// Set the button state (builder pattern).
    pub fn with_state(mut self, state: ButtonState) -> Self {
        self.state = state;
        self
    }

    /// Render the button as a styled string.
    pub fn render(&self) -> String {
        match self.state {
            ButtonState::Idle => {
                // [ Label ]
                format!("[ {} ]", self.label)
            }
            ButtonState::Hover => {
                // [ Label ] with background
                format!("{}", format!("[ {} ]", self.label).with(Color::White).on(Color::DarkGrey))
            }
            ButtonState::Clicked => {
                // [ LABEL ] with highlight
                format!("{}", format!("[ {} ]", self.label.to_uppercase()).with(Color::Black).on(Color::Green))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_button_render_idle() {
        let btn = Button::new("OK");
        assert_eq!(btn.render(), "[ OK ]");
    }

    #[test]
    fn test_button_render_hover() {
        let btn = Button::new("OK").with_state(ButtonState::Hover);
        let rendered = btn.render();
        // Check for ANSI codes (basic check)
        assert_ne!(rendered, "[ OK ]");
        assert!(rendered.contains("OK"));
    }

    #[test]
    fn test_button_render_clicked() {
        let btn = Button::new("ok").with_state(ButtonState::Clicked);
        let rendered = btn.render();
        // Check for uppercase transformation
        assert!(rendered.contains("OK"));
        assert!(!rendered.contains("ok"));
    }
}
