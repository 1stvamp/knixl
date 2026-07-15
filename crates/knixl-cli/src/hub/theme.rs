//! Shared lipgloss styles for the TUI. A single teal accent, a dim grey, and a reverse
//! highlight for the focused element.

use lipgloss::{Color, Style};

pub fn color(code: &str) -> Color {
    Color(code.to_string())
}

pub fn accent() -> Style {
    Style::new().foreground(color("6"))
}

pub fn dim() -> Style {
    Style::new().foreground(color("8"))
}

/// The focused/selected element: accent background, dark foreground.
pub fn selected() -> Style {
    Style::new().foreground(color("0")).background(color("6"))
}
