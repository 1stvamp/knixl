//! Shared bubbletea-widgets (Bubbles) building blocks, styled to the Charm palette: a
//! compact one-line list delegate, a chrome-free styled list, and a help footer built from
//! key bindings. Screens drive real components (list, help, key, spinner, textinput,
//! viewport) rather than hand-rolled state; the elements with no Bubbles equivalent (toggle,
//! buttons, value cyclers) stay bespoke.

use bubbletea_rs::{Cmd, Msg};
use bubbletea_widgets::help;
use bubbletea_widgets::key::{self, Binding};
use bubbletea_widgets::list::{DefaultItem, ItemDelegate, Model as List};

use super::theme;

/// A one-line list item renderer in the Charm palette: the selected row is drawn inverse with
/// a `▸` marker; others are dim. An item's description (if any) trails as a dim tag.
pub struct CompactDelegate;

impl ItemDelegate<DefaultItem> for CompactDelegate {
    fn render(&self, m: &List<DefaultItem>, index: usize, item: &DefaultItem) -> String {
        let title = &item.title;
        let tag = if item.desc.is_empty() {
            String::new()
        } else {
            format!(" {}", theme::dim().render(&format!("({})", item.desc)))
        };
        if index == m.cursor() {
            format!("{}{}", theme::selected().render(&format!(" \u{25b8} {title} ")), tag)
        } else {
            format!("   {}{}", theme::dim().render(title), tag)
        }
    }

    fn height(&self) -> usize {
        1
    }

    fn spacing(&self) -> usize {
        0
    }

    fn update(&self, _msg: &Msg, _m: &mut List<DefaultItem>) -> Option<Cmd> {
        None
    }
}

/// A styled, chrome-free list: no title, status bar, pagination, or built-in help, so it is
/// just the selectable rows (the surrounding screen supplies the title chip and footer).
pub fn styled_list(items: Vec<DefaultItem>, width: usize, height: usize) -> List<DefaultItem> {
    let mut list = List::new(items, CompactDelegate, width, height);
    list.set_show_title(false);
    list.set_show_status_bar(false);
    list.set_show_pagination(false);
    list.set_show_help(false);
    list
}

/// A key binding whose help text is `key` + `desc` (the keys themselves are only used for the
/// footer's label, not matched here).
pub fn binding(keycap: &str, desc: &str) -> Binding {
    key::new_binding(vec![
        key::with_keys_str(&[keycap]),
        key::with_help(keycap.to_string(), desc.to_string()),
    ])
}

/// Render a short help footer (`key desc · key desc`) from `(keycap, desc)` pairs, using the
/// Bubbles help component.
pub fn footer(pairs: &[(&str, &str)]) -> String {
    let bindings: Vec<Binding> = pairs.iter().map(|(k, d)| binding(k, d)).collect();
    let refs: Vec<&Binding> = bindings.iter().collect();
    help::Model::new().short_help_view(refs)
}
