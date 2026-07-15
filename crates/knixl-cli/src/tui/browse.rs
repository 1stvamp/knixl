//! Browse screen: a list of available modules (built-in and declarative, tagged by kind) on
//! the left, the selected module's schema doc in a scrollable viewport on the right, and an
//! `insert` action that scaffolds the module's node into a chosen host.
//!
//! Like the other screens the decision logic (selection, doc switching, the host-pick
//! sub-mode, and what an insert commits) is pure and unit tested; the viewport widget carries
//! the scrolling.

use bubbletea_rs::event::{KeyMsg, WindowSizeMsg};
use bubbletea_rs::{Model as BubbleTeaModel, Msg};
use bubbletea_widgets::viewport;
use crossterm::event::{KeyCode, KeyModifiers};
use lipgloss::{join_horizontal, join_vertical, rounded_border, Style, LEFT, TOP};

use knixl_pipeline::install::HostInfo;

use super::{config, theme, BrowseModule, Nav, Step};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    List,
    PickHost,
}

pub struct BrowseModel {
    modules: Vec<BrowseModule>,
    hosts: Vec<HostInfo>,
    sel: usize,
    host_pick: usize,
    mode: Mode,
    doc: viewport::Model,
    dims: (usize, usize),
}

fn view_dims(size: (u16, u16)) -> (usize, usize) {
    let w = (size.0 as usize / 2).saturating_sub(4).clamp(20, 70);
    let h = (size.1 as usize).saturating_sub(6).clamp(3, 24);
    (w, h)
}

fn too_small(size: (u16, u16)) -> bool {
    size.0 < 50 || size.1 < 12
}

impl BrowseModel {
    pub fn enter(size: (u16, u16)) -> BrowseModel {
        let cfg = config();
        let (w, h) = view_dims(size);
        let mut model = BrowseModel {
            modules: cfg.modules.clone(),
            hosts: cfg.hosts.clone(),
            sel: 0,
            host_pick: 0,
            mode: Mode::List,
            doc: viewport::new(w, h),
            dims: (w, h),
        };
        model.sync_doc();
        model
    }

    // ---- pure decision logic (unit tested) ----

    fn sync_doc(&mut self) {
        let text = self.modules.get(self.sel).map(|m| m.doc.clone()).unwrap_or_default();
        self.doc.set_content(&text);
    }

    fn select(&mut self, idx: usize) {
        if idx < self.modules.len() && idx != self.sel {
            self.sel = idx;
            self.doc.goto_top();
            self.sync_doc();
        }
    }

    fn resize(&mut self, size: (u16, u16)) {
        let dims = view_dims(size);
        if dims != self.dims {
            self.dims = dims;
            self.doc = viewport::new(dims.0, dims.1);
            self.sync_doc();
        }
    }

    /// The navigation intent for the currently focused action. In list mode `insert` opens
    /// the host picker (unless there are no modules or hosts); in pick mode it commits.
    fn activate(&mut self) -> Nav {
        match self.mode {
            Mode::List => {
                if self.modules.is_empty() || self.hosts.is_empty() {
                    return Nav::Stay;
                }
                self.mode = Mode::PickHost;
                self.host_pick = 0;
                Nav::Stay
            }
            Mode::PickHost => {
                let module = &self.modules[self.sel];
                Nav::Insert {
                    host: self.hosts[self.host_pick].clone(),
                    node: module.node.clone(),
                    skeleton: module.skeleton.clone(),
                }
            }
        }
    }

    pub fn update(&mut self, msg: Msg, size: (u16, u16)) -> Step {
        if msg.downcast_ref::<WindowSizeMsg>().is_some() {
            self.resize(size);
            return Step::stay();
        }
        let Some((code, mods)) = key_of(&msg) else { return Step::stay() };

        if matches!(code, KeyCode::Char('c')) && mods.contains(KeyModifiers::CONTROL) {
            return Step::nav(Nav::Back);
        }

        match self.mode {
            Mode::List => match code {
                KeyCode::Esc | KeyCode::Char('q') => Step::nav(Nav::Back),
                KeyCode::Up | KeyCode::Char('k') => {
                    self.select(self.sel.saturating_sub(1));
                    Step::stay()
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.select(self.sel + 1);
                    Step::stay()
                }
                KeyCode::Enter | KeyCode::Char('i') => Step::nav(self.activate()),
                // Everything else (PageUp/PageDown/Home/End) scrolls the doc.
                _ => {
                    let cmd = self.doc.update(msg);
                    Step { nav: Nav::Stay, cmd }
                }
            },
            Mode::PickHost => match code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.mode = Mode::List;
                    Step::stay()
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.host_pick = self.host_pick.saturating_sub(1);
                    Step::stay()
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.host_pick = (self.host_pick + 1).min(self.hosts.len().saturating_sub(1));
                    Step::stay()
                }
                KeyCode::Enter => Step::nav(self.activate()),
                _ => Step::stay(),
            },
        }
    }

    pub fn view(&self, size: (u16, u16)) -> String {
        if too_small(size) {
            return theme::dim().render("terminal too small \u{2013} resize to browse modules");
        }
        if self.modules.is_empty() {
            return format!(
                "{}\n{}",
                theme::chip(" browse "),
                theme::dim().render("no modules registered"),
            );
        }
        if self.mode == Mode::PickHost {
            return self.view_pick();
        }

        let mut items = Vec::new();
        for (i, m) in self.modules.iter().enumerate() {
            let tag = theme::dim().render(&format!(" ({})", m.kind));
            let row = format!("{}{}", m.node, tag);
            if i == self.sel {
                items.push(theme::selected().render(&format!(" \u{25b8} {} ", m.node)) + &tag);
            } else {
                items.push(format!("   {row}"));
            }
        }
        let list = join_vertical(LEFT, &items.iter().map(String::as_str).collect::<Vec<_>>());
        // The module list is the focused pane (pink border); the doc pane is violet.
        let list_box = Style::new()
            .border(rounded_border())
            .border_foreground(theme::border(true))
            .render(&list);
        let doc_box = Style::new()
            .border(rounded_border())
            .border_foreground(theme::border(false))
            .render(&self.doc.view());
        let panes = join_horizontal(TOP, &[list_box.as_str(), "  ", doc_box.as_str()]);

        let hint = theme::dim().render(
            "\u{2191}/\u{2193} select \u{00b7} i insert into host \u{00b7} pgup/pgdn scroll \u{00b7} esc back",
        );
        format!("{}\n{}\n{}", theme::chip(" browse "), panes, hint)
    }

    fn view_pick(&self) -> String {
        let node = &self.modules[self.sel].node;
        let mut rows = Vec::new();
        for (i, h) in self.hosts.iter().enumerate() {
            if i == self.host_pick {
                rows.push(theme::selected().render(&format!(" \u{25b8} {} ", h.name)));
            } else {
                rows.push(theme::dim().render(&format!("   {}", h.name)));
            }
        }
        let list = join_vertical(LEFT, &rows.iter().map(String::as_str).collect::<Vec<_>>());
        let boxed = Style::new()
            .border(rounded_border())
            .border_foreground(theme::border(true))
            .render(&list);
        format!(
            "{}\n{}\n{}",
            theme::chip(&format!(" insert {node} into ")),
            boxed,
            theme::dim().render("\u{2191}/\u{2193} pick \u{00b7} enter insert \u{00b7} esc cancel"),
        )
    }
}

fn key_of(msg: &Msg) -> Option<(KeyCode, KeyModifiers)> {
    msg.downcast_ref::<KeyMsg>().map(|k| (k.key, k.modifiers))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn host(name: &str) -> HostInfo {
        HostInfo { name: name.into(), default: false, path: PathBuf::from(format!("hosts/{name}.kdl")) }
    }

    fn module(node: &str, kind: &str) -> BrowseModule {
        BrowseModule {
            node: node.into(),
            kind: kind.into(),
            doc: format!("# {node}\n\nthe {node} module\n"),
            skeleton: node.into(),
        }
    }

    fn model(mods: &[(&str, &str)], hosts: usize) -> BrowseModel {
        let mut m = BrowseModel {
            modules: mods.iter().map(|(n, k)| module(n, k)).collect(),
            hosts: (0..hosts).map(|i| host(&format!("h{i}"))).collect(),
            sel: 0,
            host_pick: 0,
            mode: Mode::List,
            doc: viewport::new(40, 10),
            dims: (40, 10),
        };
        m.sync_doc();
        m
    }

    #[test]
    fn selecting_moves_and_updates_the_doc() {
        let mut m = model(&[("postgres", "built-in"), ("web-service", "declarative")], 1);
        assert!(m.doc.view().contains("postgres"));
        m.select(1);
        assert_eq!(m.sel, 1);
        assert!(m.doc.view().contains("web-service"), "doc follows selection");
        m.select(9);
        assert_eq!(m.sel, 1, "out-of-range selection is refused");
    }

    #[test]
    fn insert_opens_the_host_picker_then_commits() {
        let mut m = model(&[("postgres", "built-in")], 2);
        // First activate: enter the host picker.
        assert!(matches!(m.activate(), Nav::Stay));
        assert_eq!(m.mode, Mode::PickHost);
        // Pick the second host and commit.
        m.host_pick = 1;
        match m.activate() {
            Nav::Insert { host, node, skeleton } => {
                assert_eq!(host.name, "h1");
                assert_eq!(node, "postgres");
                assert_eq!(skeleton, "postgres");
            }
            _ => panic!("expected an insert"),
        }
    }

    #[test]
    fn insert_is_refused_without_hosts() {
        let mut m = model(&[("postgres", "built-in")], 0);
        assert!(matches!(m.activate(), Nav::Stay));
        assert_eq!(m.mode, Mode::List, "no hosts means no picker");
    }

    #[test]
    fn resize_rebuilds_the_doc_viewport() {
        let mut m = model(&[("postgres", "built-in")], 1);
        m.resize((160, 50));
        assert_eq!(m.dims, view_dims((160, 50)));
        assert!(m.doc.view().contains("postgres"), "doc survives a resize");
    }

    #[test]
    fn view_lists_modules_with_kind_tags() {
        let m = model(&[("postgres", "built-in"), ("web-service", "declarative")], 1);
        let v = m.view((120, 30));
        assert!(v.contains("postgres"));
        assert!(v.contains("web-service"));
        assert!(v.contains("built-in"));
        assert!(v.contains("declarative"));
    }

    #[test]
    fn view_pick_lists_hosts() {
        let mut m = model(&[("postgres", "built-in")], 2);
        m.mode = Mode::PickHost;
        let v = m.view((120, 30));
        assert!(v.contains("insert postgres"));
        assert!(v.contains("h0"));
        assert!(v.contains("h1"));
    }

    #[test]
    fn view_reports_no_modules() {
        let m = model(&[], 1);
        assert!(m.view((120, 30)).contains("no modules"));
    }

    #[test]
    fn view_shows_resize_hint_when_tiny() {
        let m = model(&[("postgres", "built-in")], 1);
        assert!(m.view((20, 6)).contains("resize"));
    }
}
