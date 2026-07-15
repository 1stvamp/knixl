//! Author screen: a small form that scaffolds a new declarative module. It collects a name,
//! the claimed node, a summary, one starting schema field (name, type, required), and writes
//! a valid `knixl-module.kdl` skeleton the author then fills in.
//!
//! The form logic (focus movement, the type cycle, the required toggle, create gating, and
//! the rendered manifest) is pure and unit tested; the textinput widgets carry editing.

use bubbletea_rs::event::{KeyMsg, WindowSizeMsg};
use bubbletea_rs::Msg;
use bubbletea_widgets::textinput;
use crossterm::event::{KeyCode, KeyModifiers};
use lipgloss::{join_vertical, rounded_border, Style, LEFT};

use knixl_modules::template::{scaffold_manifest, ModuleScaffold};

use super::{theme, Nav, Step};

const TYPES: [&str; 3] = ["string", "bool", "int"];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Focus {
    Name,
    Node,
    Summary,
    FieldName,
    FieldType,
    Required,
    Create,
    Cancel,
}

const FOCUS_ORDER: [Focus; 8] = [
    Focus::Name,
    Focus::Node,
    Focus::Summary,
    Focus::FieldName,
    Focus::FieldType,
    Focus::Required,
    Focus::Create,
    Focus::Cancel,
];

/// The four editable text fields, in focus order.
const TEXT_LABELS: [&str; 4] = ["name    ", "node    ", "summary ", "field   "];

pub struct AuthorModel {
    inputs: Vec<textinput::Model>, // name, node, summary, field name
    focus: Focus,
    field_ty: usize,
    required: bool,
}

impl AuthorModel {
    pub fn enter(_size: (u16, u16)) -> AuthorModel {
        let placeholders = ["my-module", "(defaults to name)", "what it does", "target"];
        let mut inputs = Vec::new();
        for ph in placeholders {
            let mut ti = textinput::new();
            ti.set_placeholder(ph);
            inputs.push(ti);
        }
        let mut model = AuthorModel { inputs, focus: Focus::Name, field_ty: 0, required: false };
        model.refocus();
        model
    }

    // ---- pure form logic (unit tested) ----

    fn text_index(focus: Focus) -> Option<usize> {
        match focus {
            Focus::Name => Some(0),
            Focus::Node => Some(1),
            Focus::Summary => Some(2),
            Focus::FieldName => Some(3),
            _ => None,
        }
    }

    /// Focus the widget matching the current focus (and blur the others), so only the active
    /// text field shows a cursor and takes input.
    fn refocus(&mut self) {
        let active = Self::text_index(self.focus);
        for (i, ti) in self.inputs.iter_mut().enumerate() {
            if Some(i) == active {
                std::mem::drop(ti.focus());
            } else {
                ti.blur();
            }
        }
    }

    fn focus_index(&self) -> usize {
        FOCUS_ORDER.iter().position(|f| *f == self.focus).unwrap_or(0)
    }

    fn focus_next(&mut self) {
        let i = (self.focus_index() + 1) % FOCUS_ORDER.len();
        self.focus = FOCUS_ORDER[i];
        self.refocus();
    }

    fn focus_prev(&mut self) {
        let i = (self.focus_index() + FOCUS_ORDER.len() - 1) % FOCUS_ORDER.len();
        self.focus = FOCUS_ORDER[i];
        self.refocus();
    }

    fn cycle_type(&mut self, forward: bool) {
        self.field_ty = if forward {
            (self.field_ty + 1) % TYPES.len()
        } else {
            (self.field_ty + TYPES.len() - 1) % TYPES.len()
        };
    }

    fn value(&self, i: usize) -> String {
        self.inputs[i].value()
    }

    /// Create is allowed once the module name is non-empty.
    fn can_create(&self) -> bool {
        !self.value(0).trim().is_empty()
    }

    /// Build the directory name and the manifest text from the form. The node defaults to the
    /// name, and the field name to `value`, so the scaffold is always valid.
    fn scaffold(&self) -> (String, String) {
        let name = self.value(0).trim().to_string();
        let node_in = self.value(1).trim().to_string();
        let node = if node_in.is_empty() { name.clone() } else { node_in };
        let summary = self.value(2).trim().to_string();
        let field_in = self.value(3).trim().to_string();
        let field = if field_in.is_empty() { "value".to_string() } else { field_in };
        let manifest = scaffold_manifest(&ModuleScaffold {
            name: &name,
            node: &node,
            summary: &summary,
            field_name: &field,
            field_ty: TYPES[self.field_ty],
            required: self.required,
        });
        (name, manifest)
    }

    pub fn update(&mut self, msg: Msg, _size: (u16, u16)) -> Step {
        if msg.downcast_ref::<WindowSizeMsg>().is_some() {
            return Step::stay();
        }
        let Some((code, mods)) = key_of(&msg) else { return Step::stay() };

        if matches!(code, KeyCode::Char('c')) && mods.contains(KeyModifiers::CONTROL) {
            return Step::nav(Nav::Back);
        }
        if code == KeyCode::Esc {
            return Step::nav(Nav::Back);
        }
        // Tab and the arrow keys move between fields; Left/Right stay for the type cycle.
        if matches!(code, KeyCode::Tab | KeyCode::Down) {
            self.focus_next();
            return Step::stay();
        }
        if matches!(code, KeyCode::BackTab | KeyCode::Up) {
            self.focus_prev();
            return Step::stay();
        }

        match self.focus {
            Focus::FieldType => match code {
                KeyCode::Left => {
                    self.cycle_type(false);
                    Step::stay()
                }
                KeyCode::Right | KeyCode::Char(' ') => {
                    self.cycle_type(true);
                    Step::stay()
                }
                _ => Step::stay(),
            },
            Focus::Required => match code {
                KeyCode::Char(' ') | KeyCode::Enter => {
                    self.required = !self.required;
                    Step::stay()
                }
                _ => Step::stay(),
            },
            Focus::Create => match code {
                KeyCode::Enter if self.can_create() => {
                    let (name, manifest) = self.scaffold();
                    Step::nav(Nav::Scaffold { name, manifest })
                }
                _ => Step::stay(),
            },
            Focus::Cancel => match code {
                KeyCode::Enter => Step::nav(Nav::Back),
                _ => Step::stay(),
            },
            // A text field: forward the key to its widget.
            other => match Self::text_index(other) {
                Some(i) => {
                    let cmd = self.inputs[i].update(msg);
                    Step { nav: Nav::Stay, cmd }
                }
                None => Step::stay(),
            },
        }
    }

    pub fn view(&self, size: (u16, u16)) -> String {
        if size.0 < 34 || size.1 < 14 {
            return theme::dim().render("terminal too small \u{2013} resize to author a module");
        }

        let mut lines = Vec::new();
        for (i, label) in TEXT_LABELS.iter().enumerate() {
            let focused = Self::text_index(self.focus) == Some(i);
            lines.push(format!(
                "{}{}{}",
                marker(focused),
                theme::dim().render(label),
                self.inputs[i].view(),
            ));
        }
        lines.push(format!(
            "{}{}{}{}{}",
            marker(self.focus == Focus::FieldType),
            theme::dim().render("type    "),
            theme::amber().render("\u{2039} "),
            Style::new().bold(true).render(TYPES[self.field_ty]),
            theme::amber().render(" \u{203a}"),
        ));
        lines.push(format!(
            "{}{}{} required",
            marker(self.focus == Focus::Required),
            theme::dim().render("req     "),
            if self.required { theme::accent().render("[x]") } else { theme::dim().render("[ ]") },
        ));

        let create = self.button("create", self.focus == Focus::Create, self.can_create());
        let cancel = self.button("cancel", self.focus == Focus::Cancel, true);

        let body = join_vertical(LEFT, &lines.iter().map(String::as_str).collect::<Vec<_>>());
        let panel = Style::new()
            .border(rounded_border())
            .border_foreground(theme::color("6"))
            .padding_2(0, 1)
            .render(&body);

        format!(
            "{}\n{}\n{}  {}\n{}",
            theme::accent().render(" new module "),
            panel,
            create,
            cancel,
            theme::dim().render(
                "tab move \u{00b7} \u{2190}/\u{2192} type \u{00b7} space required \u{00b7} enter create \u{00b7} esc back",
            ),
        )
    }

    fn button(&self, label: &str, focused: bool, enabled: bool) -> String {
        let text = format!(" {label} ");
        if focused {
            theme::selected().render(&text)
        } else if enabled {
            theme::accent().render(&text)
        } else {
            theme::dim().render(&text)
        }
    }
}

fn marker(focused: bool) -> String {
    if focused {
        theme::accent().render("\u{25b8} ")
    } else {
        "  ".to_string()
    }
}

fn key_of(msg: &Msg) -> Option<(KeyCode, KeyModifiers)> {
    msg.downcast_ref::<KeyMsg>().map(|k| (k.key, k.modifiers))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> AuthorModel {
        AuthorModel::enter((80, 24))
    }

    #[test]
    fn focus_cycles_and_wraps() {
        let mut m = model();
        assert_eq!(m.focus, Focus::Name);
        m.focus_prev();
        assert_eq!(m.focus, Focus::Cancel, "wraps to the last control");
        m.focus_next();
        assert_eq!(m.focus, Focus::Name);
    }

    #[test]
    fn arrow_keys_move_between_fields() {
        let mut m = model();
        assert_eq!(m.focus, Focus::Name);
        let key = |c| Box::new(KeyMsg { key: c, modifiers: KeyModifiers::NONE }) as Msg;
        m.update(key(KeyCode::Down), (80, 24));
        assert_eq!(m.focus, Focus::Node, "down moves to the next field");
        m.update(key(KeyCode::Up), (80, 24));
        assert_eq!(m.focus, Focus::Name, "up moves back");
    }

    #[test]
    fn type_cycles_both_ways() {
        let mut m = model();
        assert_eq!(TYPES[m.field_ty], "string");
        m.cycle_type(true);
        assert_eq!(TYPES[m.field_ty], "bool");
        m.cycle_type(false);
        assert_eq!(TYPES[m.field_ty], "string");
        m.cycle_type(false);
        assert_eq!(TYPES[m.field_ty], "int", "wraps backwards");
    }

    #[test]
    fn create_is_gated_on_a_name() {
        let mut m = model();
        assert!(!m.can_create(), "empty name blocks create");
        m.inputs[0].set_value("my-mod");
        assert!(m.can_create());
    }

    #[test]
    fn scaffold_defaults_node_to_name_and_carries_the_field() {
        let mut m = model();
        m.inputs[0].set_value("cache");
        // node left blank -> defaults to name.
        m.inputs[2].set_value("a caching module");
        m.inputs[3].set_value("size");
        m.cycle_type(true); // -> bool
        m.required = true;
        let (name, manifest) = m.scaffold();
        assert_eq!(name, "cache");
        assert!(manifest.contains("claims-node \"cache\""), "node defaults to name: {manifest}");
        assert!(manifest.contains("a caching module"));
        assert!(manifest.contains("arg \"size\" type=\"bool\" required=#true"), "{manifest}");
    }

    #[test]
    fn scaffold_uses_an_explicit_node_when_given() {
        let mut m = model();
        m.inputs[0].set_value("cache");
        m.inputs[1].set_value("kv-cache");
        let (_name, manifest) = m.scaffold();
        assert!(manifest.contains("claims-node \"kv-cache\""), "{manifest}");
    }

    #[test]
    fn view_shows_the_form_fields() {
        let v = model().view((80, 24));
        assert!(v.contains("new module"));
        assert!(v.contains("type"));
        assert!(v.contains("required"));
        assert!(v.contains("create"));
        assert!(v.contains("cancel"));
    }

    #[test]
    fn view_shows_resize_hint_when_tiny() {
        assert!(model().view((20, 8)).contains("resize"));
    }
}
