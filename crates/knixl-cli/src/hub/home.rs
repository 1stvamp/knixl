//! Home screen: a focus-navigable menu that routes to the other screens.

use bubbletea_rs::event::KeyMsg;
use crossterm::event::{KeyCode, KeyModifiers};
use lipgloss::{join_vertical, rounded_border, Style, LEFT};

use super::{theme, Nav};

/// (label, screen key). The key routes in `App::apply`.
const ITEMS: &[(&str, &str)] = &[
    ("Install a package", "install"),
    ("Browse modules", "browse"),
    ("New module", "author"),
    ("Quit", "quit"),
];

pub struct HomeModel {
    pub sel: usize,
}

impl HomeModel {
    pub fn new() -> Self {
        Self { sel: 0 }
    }

    /// Pure reducer: fold a key into the selection and report navigation intent.
    pub fn update(&mut self, key: &KeyMsg) -> Nav {
        match key.key {
            KeyCode::Up | KeyCode::Char('k') => {
                self.sel = self.sel.saturating_sub(1);
                Nav::Stay
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.sel = (self.sel + 1).min(ITEMS.len() - 1);
                Nav::Stay
            }
            KeyCode::Enter => match ITEMS[self.sel].1 {
                "quit" => Nav::Quit,
                other => Nav::Goto(other),
            },
            KeyCode::Esc | KeyCode::Char('q') => Nav::Quit,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => Nav::Quit,
            _ => Nav::Stay,
        }
    }

    pub fn view(&self, _size: (u16, u16)) -> String {
        let mut lines = Vec::new();
        for (i, (label, _)) in ITEMS.iter().enumerate() {
            if i == self.sel {
                lines.push(theme::selected().render(&format!(" \u{25b8} {label} ")));
            } else {
                lines.push(theme::dim().render(&format!("   {label}")));
            }
        }
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let body = join_vertical(LEFT, &refs);
        let panel = Style::new()
            .border(rounded_border())
            .border_foreground(theme::color("6"))
            .padding_2(0, 1);
        format!(
            "{}\n{}\n{}",
            theme::accent().render(" knixl "),
            panel.render(&body),
            theme::dim().render("\u{2191}/\u{2193} move  \u{00b7} enter select  \u{00b7} q quit"),
        )
    }
}
