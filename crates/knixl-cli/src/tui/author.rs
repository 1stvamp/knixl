//! Author screen: a full editor for a declarative module manifest. It collects a name, the
//! claimed node, a summary, a structured schema (a list of arg/prop/child entries, each with a
//! type, required/repeated flags, and, for children, its own sub-fields), and a free-text emit
//! block. The draft is rendered to KDL and dry-type-checked on every edit, so the status line
//! and the create gate stay live.
//!
//! The form logic (focus movement, the schema mutations, create gating, and the rendered
//! manifest) is pure and unit tested; the textinput/textarea widgets carry editing.

use bubbletea_rs::event::{KeyMsg, WindowSizeMsg};
use bubbletea_rs::Msg;
use bubbletea_widgets::{textarea, textinput, Component};
use crossterm::event::{KeyCode, KeyModifiers};
use lipgloss::{join_vertical, rounded_border, Style, LEFT};

use knixl_modules::template::{
    render_manifest, validate_manifest, EntryKind, FieldTy, ModuleDraft, SchemaEntry, SubField,
    SubKind,
};

use super::{theme, widgets, Nav, Step};

/// A point in the form the user can move focus to. Computed on demand from the current
/// entries/subfields, never stored as a big vector on the model, so it always matches the
/// live shape of the draft.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Focus {
    Name,
    Node,
    Summary,
    EntryKind(usize),
    EntryName(usize),
    EntryType(usize),
    EntryRequired(usize),
    EntryRepeated(usize),
    SubKind(usize, usize),
    SubName(usize, usize),
    SubType(usize, usize),
    SubRequired(usize, usize),
    AddSub(usize),
    DeleteEntry(usize),
    AddEntry,
    Emit,
    Create,
    Cancel,
}

struct SubFieldState {
    kind: SubKind,
    name: textinput::Model,
    ty: FieldTy,
    required: bool,
}

struct EntryState {
    kind: EntryKind,
    name: textinput::Model,
    ty: FieldTy,
    required: bool,
    repeated: bool,
    subfields: Vec<SubFieldState>,
}

pub struct AuthorModel {
    name: textinput::Model,
    node: textinput::Model,
    summary: textinput::Model,
    entries: Vec<EntryState>,
    emit: textarea::Model,
    focus: usize,               // index into the computed focus list
    status: Result<(), String>, // cached validation of the current draft
    /// Cached `emit.view()` output. `textarea::Model::view` takes `&mut self` (it maintains a
    /// rendering cache internally) but `AuthorModel::view` must stay `&self` to match the
    /// screen's public contract, so the rendered text is captured here every time the emit
    /// widget could have changed (edits, resize, focus changes) and `view` just reads it back.
    emit_view: String,
}

impl AuthorModel {
    pub fn enter(size: (u16, u16)) -> AuthorModel {
        let mut name = textinput::new();
        name.set_placeholder("my-module");
        let mut node = textinput::new();
        node.set_placeholder("(defaults to name)");
        let mut summary = textinput::new();
        summary.set_placeholder("what it does");

        let mut field_name = textinput::new();
        field_name.set_placeholder("field");
        let seed = EntryState {
            kind: EntryKind::Arg,
            name: field_name,
            ty: FieldTy::Str,
            required: false,
            repeated: false,
            subfields: Vec::new(),
        };

        let mut emit = textarea::new();
        emit.set_value("set \"services.new-module.enable\" #true");

        let mut model = AuthorModel {
            name,
            node,
            summary,
            entries: vec![seed],
            emit,
            focus: 0,
            status: Ok(()),
            emit_view: String::new(),
        };
        model.size_emit(size);
        model.refocus();
        model.recompute_status();
        model
    }

    // ---- focus list (computed on demand) ----

    fn focus_list(&self) -> Vec<Focus> {
        let mut list = vec![Focus::Name, Focus::Node, Focus::Summary];
        for (i, e) in self.entries.iter().enumerate() {
            list.push(Focus::EntryKind(i));
            list.push(Focus::EntryName(i));
            list.push(Focus::EntryType(i));
            list.push(Focus::EntryRequired(i));
            if e.kind == EntryKind::Child {
                list.push(Focus::EntryRepeated(i));
                for j in 0..e.subfields.len() {
                    list.push(Focus::SubKind(i, j));
                    list.push(Focus::SubName(i, j));
                    list.push(Focus::SubType(i, j));
                    list.push(Focus::SubRequired(i, j));
                }
                list.push(Focus::AddSub(i));
            }
            list.push(Focus::DeleteEntry(i));
        }
        list.push(Focus::AddEntry);
        list.push(Focus::Emit);
        list.push(Focus::Create);
        list.push(Focus::Cancel);
        list
    }

    fn focus_at(&self) -> Focus {
        let list = self.focus_list();
        list[self.focus.min(list.len() - 1)]
    }

    fn focus_next(&mut self) {
        let len = self.focus_list().len();
        self.focus = (self.focus + 1) % len;
        self.refocus();
        self.recompute_status();
    }

    fn focus_prev(&mut self) {
        let len = self.focus_list().len();
        self.focus = (self.focus + len - 1) % len;
        self.refocus();
        self.recompute_status();
    }

    /// Focus the widget matching the current focus and blur the rest, so only the active
    /// text field or the emit editor shows a cursor and takes input.
    fn refocus(&mut self) {
        let at = self.focus_at();
        self.name.blur();
        self.node.blur();
        self.summary.blur();
        for e in self.entries.iter_mut() {
            e.name.blur();
            for s in e.subfields.iter_mut() {
                s.name.blur();
            }
        }
        self.emit.blur();
        match at {
            Focus::Name => std::mem::drop(self.name.focus()),
            Focus::Node => std::mem::drop(self.node.focus()),
            Focus::Summary => std::mem::drop(self.summary.focus()),
            Focus::EntryName(i) => std::mem::drop(self.entries[i].name.focus()),
            Focus::SubName(i, j) => std::mem::drop(self.entries[i].subfields[j].name.focus()),
            Focus::Emit => std::mem::drop(self.emit.focus()),
            _ => {}
        }
    }

    /// After a mutation that can change the length of the focus list (add/delete entry or
    /// subfield, or a kind cycle that gains/loses the child-only cells), clamp focus back into
    /// range and refresh the widget focus and validation.
    fn touched(&mut self) {
        let len = self.focus_list().len();
        if self.focus >= len {
            self.focus = len.saturating_sub(1);
        }
        self.refocus();
        self.recompute_status();
    }

    fn size_emit(&mut self, size: (u16, u16)) {
        let width = (size.0 as usize).saturating_sub(10).max(20);
        let height = ((size.1 as usize) / 4).clamp(3, 10);
        self.emit.set_width(width);
        self.emit.set_height(height);
    }

    // ---- pure mutations (unit tested) ----

    fn add_entry(&mut self) {
        let mut name = textinput::new();
        name.set_placeholder("field");
        self.entries.push(EntryState {
            kind: EntryKind::Arg,
            name,
            ty: FieldTy::Str,
            required: false,
            repeated: false,
            subfields: Vec::new(),
        });
        self.touched();
    }

    fn delete_entry(&mut self, i: usize) {
        if i < self.entries.len() {
            self.entries.remove(i);
        }
        self.touched();
    }

    fn add_subfield(&mut self, i: usize) {
        let Some(entry) = self.entries.get_mut(i) else {
            return;
        };
        let mut name = textinput::new();
        name.set_placeholder("subfield");
        entry.subfields.push(SubFieldState {
            kind: SubKind::Arg,
            name,
            ty: FieldTy::Str,
            required: false,
        });
        self.touched();
    }

    fn delete_subfield(&mut self, i: usize, j: usize) {
        if let Some(entry) = self.entries.get_mut(i) {
            if j < entry.subfields.len() {
                entry.subfields.remove(j);
            }
        }
        self.touched();
    }

    fn cycle_entry_kind(&mut self, i: usize, forward: bool) {
        let Some(entry) = self.entries.get_mut(i) else {
            return;
        };
        entry.kind = cycle(
            &[EntryKind::Arg, EntryKind::Prop, EntryKind::Child],
            entry.kind,
            forward,
        );
        self.touched();
    }

    fn cycle_entry_type(&mut self, i: usize, forward: bool) {
        let Some(entry) = self.entries.get_mut(i) else {
            return;
        };
        entry.ty = cycle(
            &[FieldTy::Str, FieldTy::Bool, FieldTy::Int],
            entry.ty,
            forward,
        );
        self.recompute_status();
    }

    fn toggle_entry_required(&mut self, i: usize) {
        if let Some(entry) = self.entries.get_mut(i) {
            entry.required = !entry.required;
        }
        self.recompute_status();
    }

    fn toggle_entry_repeated(&mut self, i: usize) {
        if let Some(entry) = self.entries.get_mut(i) {
            entry.repeated = !entry.repeated;
        }
        self.recompute_status();
    }

    fn cycle_sub_kind(&mut self, i: usize, j: usize, forward: bool) {
        let Some(sub) = self.entries.get_mut(i).and_then(|e| e.subfields.get_mut(j)) else {
            return;
        };
        sub.kind = cycle(&[SubKind::Arg, SubKind::Prop], sub.kind, forward);
        self.recompute_status();
    }

    fn cycle_sub_type(&mut self, i: usize, j: usize, forward: bool) {
        let Some(sub) = self.entries.get_mut(i).and_then(|e| e.subfields.get_mut(j)) else {
            return;
        };
        sub.ty = cycle(
            &[FieldTy::Str, FieldTy::Bool, FieldTy::Int],
            sub.ty,
            forward,
        );
        self.recompute_status();
    }

    fn toggle_sub_required(&mut self, i: usize, j: usize) {
        if let Some(sub) = self.entries.get_mut(i).and_then(|e| e.subfields.get_mut(j)) {
            sub.required = !sub.required;
        }
        self.recompute_status();
    }

    /// Build the `ModuleDraft` the widgets currently describe.
    fn draft(&self) -> ModuleDraft {
        ModuleDraft {
            name: self.name.value().trim().to_string(),
            node: self.node.value().trim().to_string(),
            summary: self.summary.value().trim().to_string(),
            entries: self
                .entries
                .iter()
                .map(|e| SchemaEntry {
                    kind: e.kind,
                    name: e.name.value().trim().to_string(),
                    ty: e.ty,
                    required: e.required,
                    repeated: e.repeated,
                    subfields: e
                        .subfields
                        .iter()
                        .map(|s| SubField {
                            kind: s.kind,
                            name: s.name.value().trim().to_string(),
                            ty: s.ty,
                            required: s.required,
                        })
                        .collect(),
                })
                .collect(),
            emit: self.emit.value(),
        }
    }

    /// Re-render the draft to KDL and dry-type-check it, caching both the validation result
    /// and the emit widget's rendered text (see `emit_view`'s doc comment).
    fn recompute_status(&mut self) {
        self.status = validate_manifest(&render_manifest(&self.draft()));
        self.emit_view = self.emit.view();
    }

    /// Create is allowed once the module name is non-empty and the draft dry-type-checks.
    fn can_create(&self) -> bool {
        !self.name.value().trim().is_empty() && self.status.is_ok()
    }

    // ---- reducer ----

    pub fn update(&mut self, msg: Msg, size: (u16, u16)) -> Step {
        if msg.downcast_ref::<WindowSizeMsg>().is_some() {
            self.size_emit(size);
            self.recompute_status();
            return Step::stay();
        }
        let Some((code, mods)) = key_of(&msg) else {
            return Step::stay();
        };

        if matches!(code, KeyCode::Char('c')) && mods.contains(KeyModifiers::CONTROL) {
            return Step::nav(Nav::Back);
        }
        if code == KeyCode::Esc {
            return Step::nav(Nav::Back);
        }

        let at = self.focus_at();

        // The emit editor owns Up/Down for cursor movement; only Tab/BackTab leave it.
        if at == Focus::Emit && matches!(code, KeyCode::Up | KeyCode::Down) {
            let cmd = self.emit.update(Some(msg));
            self.recompute_status();
            return Step {
                nav: Nav::Stay,
                cmd,
            };
        }

        if matches!(code, KeyCode::Tab | KeyCode::Down) {
            self.focus_next();
            return Step::stay();
        }
        if matches!(code, KeyCode::BackTab | KeyCode::Up) {
            self.focus_prev();
            return Step::stay();
        }

        match at {
            Focus::EntryKind(i) => {
                match code {
                    KeyCode::Left => self.cycle_entry_kind(i, false),
                    KeyCode::Right | KeyCode::Char(' ') => self.cycle_entry_kind(i, true),
                    _ => {}
                }
                Step::stay()
            }
            Focus::EntryType(i) => {
                match code {
                    KeyCode::Left => self.cycle_entry_type(i, false),
                    KeyCode::Right | KeyCode::Char(' ') => self.cycle_entry_type(i, true),
                    _ => {}
                }
                Step::stay()
            }
            Focus::EntryRequired(i) => {
                if matches!(code, KeyCode::Char(' ') | KeyCode::Enter) {
                    self.toggle_entry_required(i);
                }
                Step::stay()
            }
            Focus::EntryRepeated(i) => {
                if matches!(code, KeyCode::Char(' ') | KeyCode::Enter) {
                    self.toggle_entry_repeated(i);
                }
                Step::stay()
            }
            Focus::SubKind(i, j) => {
                match code {
                    KeyCode::Left => self.cycle_sub_kind(i, j, false),
                    KeyCode::Right | KeyCode::Char(' ') => self.cycle_sub_kind(i, j, true),
                    _ => {}
                }
                Step::stay()
            }
            Focus::SubType(i, j) => {
                match code {
                    KeyCode::Left => self.cycle_sub_type(i, j, false),
                    KeyCode::Right | KeyCode::Char(' ') => self.cycle_sub_type(i, j, true),
                    _ => {}
                }
                Step::stay()
            }
            Focus::SubRequired(i, j) => {
                if matches!(code, KeyCode::Char(' ') | KeyCode::Enter) {
                    self.toggle_sub_required(i, j);
                }
                Step::stay()
            }
            // Enter adds a sub-field; Backspace/Delete removes the last one. There is no
            // separate per-subfield delete control (see the view spec), so subfields come off
            // the end of the list from here.
            Focus::AddSub(i) => {
                match code {
                    KeyCode::Enter => self.add_subfield(i),
                    KeyCode::Backspace | KeyCode::Delete => {
                        let last = self.entries.get(i).map(|e| e.subfields.len()).unwrap_or(0);
                        if last > 0 {
                            self.delete_subfield(i, last - 1);
                        }
                    }
                    _ => {}
                }
                Step::stay()
            }
            Focus::DeleteEntry(i) => {
                if code == KeyCode::Enter {
                    self.delete_entry(i);
                }
                Step::stay()
            }
            Focus::AddEntry => {
                if code == KeyCode::Enter {
                    self.add_entry();
                }
                Step::stay()
            }
            Focus::Create => match code {
                KeyCode::Enter if self.can_create() => {
                    let name = self.name.value().trim().to_string();
                    let manifest = render_manifest(&self.draft());
                    Step::nav(Nav::Scaffold { name, manifest })
                }
                _ => Step::stay(),
            },
            Focus::Cancel => match code {
                KeyCode::Enter => Step::nav(Nav::Back),
                _ => Step::stay(),
            },
            Focus::Emit => {
                let cmd = self.emit.update(Some(msg));
                self.recompute_status();
                Step {
                    nav: Nav::Stay,
                    cmd,
                }
            }
            Focus::Name => {
                let cmd = self.name.update(msg);
                self.recompute_status();
                Step {
                    nav: Nav::Stay,
                    cmd,
                }
            }
            Focus::Node => {
                let cmd = self.node.update(msg);
                self.recompute_status();
                Step {
                    nav: Nav::Stay,
                    cmd,
                }
            }
            Focus::Summary => {
                let cmd = self.summary.update(msg);
                self.recompute_status();
                Step {
                    nav: Nav::Stay,
                    cmd,
                }
            }
            Focus::EntryName(i) => {
                let cmd = self.entries[i].name.update(msg);
                self.recompute_status();
                Step {
                    nav: Nav::Stay,
                    cmd,
                }
            }
            Focus::SubName(i, j) => {
                let cmd = self.entries[i].subfields[j].name.update(msg);
                self.recompute_status();
                Step {
                    nav: Nav::Stay,
                    cmd,
                }
            }
        }
    }

    // ---- view ----

    pub fn view(&self, size: (u16, u16)) -> String {
        if size.0 < 40 || size.1 < 18 {
            return theme::dim().render("terminal too small \u{2013} resize to author a module");
        }

        let at = self.focus_at();
        let mut lines = vec![
            header_row("name    ", &self.name, at == Focus::Name),
            header_row("node    ", &self.node, at == Focus::Node),
            header_row("summary ", &self.summary, at == Focus::Summary),
            String::new(),
            theme::dim().render("schema"),
        ];
        for (i, entry) in self.entries.iter().enumerate() {
            lines.push(entry_row(at, i, entry));
            if entry.kind == EntryKind::Child {
                for (j, sub) in entry.subfields.iter().enumerate() {
                    lines.push(subfield_row(at, i, j, sub));
                }
                lines.push(control_row(
                    "    ",
                    "+ add sub-field",
                    at == Focus::AddSub(i),
                ));
            }
            lines.push(control_row(
                "  ",
                "- delete entry",
                at == Focus::DeleteEntry(i),
            ));
        }
        lines.push(control_row("", "+ add entry", at == Focus::AddEntry));

        lines.push(String::new());
        lines.push(theme::dim().render("emit"));
        let emit_panel = Style::new()
            .border(rounded_border())
            .border_foreground(theme::border(at == Focus::Emit))
            .padding_2(0, 1)
            .render(&self.emit_view);
        lines.push(format!("{}{}", marker(at == Focus::Emit), emit_panel));

        lines.push(match &self.status {
            Ok(()) => theme::dim().render("valid"),
            Err(e) => theme::bad().render(e),
        });

        let create = self.button("create", at == Focus::Create, self.can_create());
        let cancel = self.button("cancel", at == Focus::Cancel, true);

        let body = join_vertical(LEFT, &lines.iter().map(String::as_str).collect::<Vec<_>>());
        let panel = Style::new()
            .border(rounded_border())
            .border_foreground(theme::border(false))
            .padding_2(0, 1)
            .render(&body);

        format!(
            "{}\n{}\n{}  {}\n{}",
            theme::chip(" new module "),
            panel,
            create,
            cancel,
            widgets::footer(&[
                ("tab", "move"),
                ("\u{2190}/\u{2192}", "cycle"),
                ("space", "toggle"),
                ("enter", "edit/create"),
                ("esc", "back"),
            ]),
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

/// Cycle `current` to the next (or, going backward, the previous) value in `order`, wrapping.
fn cycle<T: Copy + PartialEq>(order: &[T], current: T, forward: bool) -> T {
    let n = order.len();
    let i = order.iter().position(|x| *x == current).unwrap_or(0);
    order[if forward {
        (i + 1) % n
    } else {
        (i + n - 1) % n
    }]
}

fn kind_label(k: EntryKind) -> &'static str {
    match k {
        EntryKind::Arg => "arg",
        EntryKind::Prop => "prop",
        EntryKind::Child => "child",
    }
}

fn sub_kind_label(k: SubKind) -> &'static str {
    match k {
        SubKind::Arg => "arg",
        SubKind::Prop => "prop",
    }
}

fn ty_label(t: FieldTy) -> &'static str {
    match t {
        FieldTy::Str => "string",
        FieldTy::Bool => "bool",
        FieldTy::Int => "int",
    }
}

fn header_row(label: &str, ti: &textinput::Model, focused: bool) -> String {
    format!(
        "{}{}{}",
        marker(focused),
        theme::dim().render(label),
        ti.view()
    )
}

fn cycled_cell(label: &str, focused: bool) -> String {
    let text = format!("{}\u{2039} {label} \u{203a}", marker(focused));
    theme::amber().render(&text)
}

fn entry_row(at: Focus, i: usize, e: &EntryState) -> String {
    let mut parts = vec![
        cycled_cell(kind_label(e.kind), at == Focus::EntryKind(i)),
        format!("{}{}", marker(at == Focus::EntryName(i)), e.name.view()),
        cycled_cell(ty_label(e.ty), at == Focus::EntryType(i)),
        format!(
            "{}{} required",
            marker(at == Focus::EntryRequired(i)),
            theme::toggle(e.required)
        ),
    ];
    if e.kind == EntryKind::Child {
        parts.push(format!(
            "{}{} repeated",
            marker(at == Focus::EntryRepeated(i)),
            theme::toggle(e.repeated),
        ));
    }
    parts.join("  ")
}

fn subfield_row(at: Focus, i: usize, j: usize, s: &SubFieldState) -> String {
    let parts = [
        cycled_cell(sub_kind_label(s.kind), at == Focus::SubKind(i, j)),
        format!("{}{}", marker(at == Focus::SubName(i, j)), s.name.view()),
        cycled_cell(ty_label(s.ty), at == Focus::SubType(i, j)),
        format!(
            "{}{} required",
            marker(at == Focus::SubRequired(i, j)),
            theme::toggle(s.required)
        ),
    ];
    format!("    {}", parts.join("  "))
}

fn control_row(indent: &str, label: &str, focused: bool) -> String {
    format!("{indent}{}{}", marker(focused), theme::dim().render(label))
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
        AuthorModel::enter((100, 40))
    }
    fn key(c: KeyCode) -> Msg {
        Box::new(KeyMsg {
            key: c,
            modifiers: KeyModifiers::NONE,
        }) as Msg
    }
    fn press(m: &mut AuthorModel, c: KeyCode) {
        m.update(key(c), (100, 40));
    }

    #[test]
    fn starts_with_a_seed_entry_and_emit() {
        let m = model();
        assert_eq!(m.entries.len(), 1, "one seed entry");
        assert!(!m.emit.value().trim().is_empty(), "seed emit line present");
    }

    #[test]
    fn focus_moves_through_the_dynamic_list_and_wraps() {
        let mut m = model();
        assert_eq!(m.focus_at(), Focus::Name);
        // Tab from the last control wraps to the first.
        let last = m.focus_list().len() - 1;
        m.focus = last;
        press(&mut m, KeyCode::Tab);
        assert_eq!(m.focus, 0);
        assert_eq!(m.focus_at(), Focus::Name);
    }

    #[test]
    fn add_and_delete_entry() {
        let mut m = model();
        let before = m.entries.len();
        m.add_entry();
        assert_eq!(m.entries.len(), before + 1);
        m.delete_entry(before); // remove the one just added
        assert_eq!(m.entries.len(), before);
    }

    #[test]
    fn add_subfield_only_on_a_child_and_delete() {
        let mut m = model();
        m.entries[0].kind = EntryKind::Child;
        m.add_subfield(0);
        assert_eq!(m.entries[0].subfields.len(), 1);
        m.delete_subfield(0, 0);
        assert!(m.entries[0].subfields.is_empty());
    }

    #[test]
    fn cycles_and_toggles() {
        let mut m = model();
        m.entries[0].ty = FieldTy::Str;
        m.cycle_entry_type(0, true);
        assert_eq!(m.entries[0].ty, FieldTy::Bool);
        m.cycle_entry_type(0, false);
        assert_eq!(m.entries[0].ty, FieldTy::Str);
        let r = m.entries[0].required;
        m.toggle_entry_required(0);
        assert_eq!(m.entries[0].required, !r);
        m.entries[0].kind = EntryKind::Arg;
        m.cycle_entry_kind(0, true);
        assert_eq!(m.entries[0].kind, EntryKind::Prop);
    }

    #[test]
    fn draft_reflects_the_editor_state() {
        let mut m = model();
        m.name.set_value("cache");
        m.entries[0].kind = EntryKind::Arg;
        m.entries[0].name.set_value("host");
        m.entries[0].ty = FieldTy::Str;
        let d = m.draft();
        assert_eq!(d.name, "cache");
        assert_eq!(d.entries[0].name, "host");
    }

    #[test]
    fn create_gated_on_name_and_validity() {
        let mut m = model();
        m.name.set_value(""); // no name
        m.recompute_status();
        assert!(!m.can_create(), "empty name blocks create");
        m.name.set_value("cache");
        m.entries[0].name.set_value("host");
        m.recompute_status();
        assert!(
            m.can_create(),
            "valid named draft can create: {:?}",
            m.status
        );
    }

    #[test]
    fn create_emits_scaffold_with_a_valid_manifest() {
        let mut m = model();
        m.name.set_value("cache");
        m.entries[0].name.set_value("host");
        m.recompute_status();
        // Move focus to Create and press Enter.
        let idx = m
            .focus_list()
            .iter()
            .position(|f| *f == Focus::Create)
            .unwrap();
        m.focus = idx;
        let step = m.update(key(KeyCode::Enter), (100, 40));
        match step.nav {
            Nav::Scaffold { name, manifest } => {
                assert_eq!(name, "cache");
                knixl_modules::template::validate_manifest(&manifest)
                    .expect("emitted manifest valid");
            }
            _ => panic!("expected Nav::Scaffold"),
        }
    }

    #[test]
    fn esc_backs_out() {
        let mut m = model();
        let step = m.update(key(KeyCode::Esc), (100, 40));
        assert!(matches!(step.nav, Nav::Back));
    }

    #[test]
    fn view_shows_sections_and_resize_hint() {
        assert!(model().view((100, 40)).contains("new module"));
        assert!(model().view((100, 40)).contains("schema"));
        assert!(model().view((100, 40)).contains("emit"));
        assert!(model().view((20, 8)).contains("resize"));
    }
}
