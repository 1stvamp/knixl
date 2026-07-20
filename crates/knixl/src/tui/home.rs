//! Home screen: the hub menu, driven by the Bubbles `list` component. Selecting an entry
//! routes to the matching screen.

use bubbletea_rs::event::KeyMsg;
use bubbletea_rs::{Model as BubbleTeaModel, Msg};
use bubbletea_widgets::list::{DefaultItem, Model as List};
use crossterm::event::{KeyCode, KeyModifiers};
use lipgloss::{rounded_border, Style};

use super::{theme, widgets, Nav, Step};

/// (menu label, screen key). The key routes in `App::apply`.
const ITEMS: &[(&str, &str)] = &[
    ("Install a package", "install"),
    ("Browse modules", "browse"),
    ("New module", "author"),
    ("Quit", "quit"),
];

/// Fixed menu width so the panel border does not shrink or grow with the selected row.
const MENU_WIDTH: usize = 22;

pub struct HomeModel {
    list: List<DefaultItem>,
}

impl HomeModel {
    pub fn new() -> Self {
        let items = ITEMS
            .iter()
            .map(|(label, _)| DefaultItem::new(label, ""))
            .collect();
        // Height comfortably exceeds the item count so the list never paginates or scrolls
        // (all entries stay visible; up/down just clamp at the ends).
        Self {
            list: widgets::styled_list(items, MENU_WIDTH, 12),
        }
    }

    fn route_for(label: &str) -> &'static str {
        ITEMS
            .iter()
            .find(|(l, _)| *l == label)
            .map(|(_, k)| *k)
            .unwrap_or("quit")
    }

    /// Reducer: navigation keys drive the list; Enter routes the selection; q/Esc quit.
    pub fn update(&mut self, msg: Msg, size: (u16, u16)) -> Step {
        let Some(key) = msg.downcast_ref::<KeyMsg>() else {
            let cmd = self.list.update(msg);
            return Step {
                nav: Nav::Stay,
                cmd,
            };
        };
        let (code, mods) = (key.key, key.modifiers);
        let _ = size;
        match code {
            KeyCode::Enter => {
                let route = self
                    .list
                    .selected_item()
                    .map(|i| Self::route_for(&i.title))
                    .unwrap_or("quit");
                match route {
                    "quit" => Step::nav(Nav::Quit),
                    other => Step::nav(Nav::Goto(other)),
                }
            }
            KeyCode::Esc | KeyCode::Char('q') => Step::nav(Nav::Quit),
            KeyCode::Char('c') if mods.contains(KeyModifiers::CONTROL) => Step::nav(Nav::Quit),
            _ => {
                let cmd = self.list.update(msg);
                Step {
                    nav: Nav::Stay,
                    cmd,
                }
            }
        }
    }

    pub fn view(&self, size: (u16, u16)) -> String {
        let panel = Style::new()
            .border(rounded_border())
            .border_foreground(theme::border(false))
            .width(MENU_WIDTH as i32)
            .render(&self.list.view());
        let header = if size.0 >= 46 {
            theme::wordmark()
        } else {
            theme::chip(" knixl ")
        };
        format!(
            "{}\n{}\n{}\n{}",
            header,
            theme::dim().render("opinionated nix, made legible"),
            panel,
            widgets::footer(&[
                ("\u{2191}/\u{2193}", "move"),
                ("enter", "select"),
                ("q", "quit")
            ]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> Msg {
        Box::new(KeyMsg {
            key: code,
            modifiers: KeyModifiers::NONE,
        }) as Msg
    }

    #[test]
    fn down_moves_the_list_cursor() {
        let mut h = HomeModel::new();
        assert_eq!(h.list.cursor(), 0);
        h.update(key(KeyCode::Down), (80, 24));
        assert_eq!(h.list.cursor(), 1);
    }

    #[test]
    fn enter_routes_the_selection() {
        let mut h = HomeModel::new();
        assert!(matches!(
            h.update(key(KeyCode::Enter), (80, 24)).nav,
            Nav::Goto("install")
        ));
        for _ in 0..3 {
            h.update(key(KeyCode::Down), (80, 24));
        }
        assert!(matches!(
            h.update(key(KeyCode::Enter), (80, 24)).nav,
            Nav::Quit
        ));
    }

    #[test]
    fn q_and_esc_quit() {
        let mut h = HomeModel::new();
        assert!(matches!(
            h.update(key(KeyCode::Char('q')), (80, 24)).nav,
            Nav::Quit
        ));
        assert!(matches!(
            h.update(key(KeyCode::Esc), (80, 24)).nav,
            Nav::Quit
        ));
    }

    #[test]
    fn view_lists_the_menu() {
        let v = HomeModel::new().view((80, 24));
        for item in ["Install a package", "Browse modules", "New module", "Quit"] {
            assert!(v.contains(item), "menu shows {item}: {v}");
        }
    }
}
