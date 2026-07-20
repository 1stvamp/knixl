//! Browse screen: a Bubbles `list` of modules (tagged by kind) on the left, the selected
//! module's schema doc in a `viewport` on the right, and an `insert` action that opens a
//! second `list` to pick a host and scaffolds the module's node into it.

use bubbletea_rs::event::{KeyMsg, WindowSizeMsg};
use bubbletea_rs::{Model as BubbleTeaModel, Msg};
use bubbletea_widgets::list::{DefaultItem, Model as List};
use bubbletea_widgets::viewport;
use crossterm::event::{KeyCode, KeyModifiers};
use lipgloss::{join_horizontal, rounded_border, Style, TOP};

use knixl_pipeline::install::HostInfo;

use super::{config, theme, widgets, BrowseModule, Nav, Step};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    List,
    PickHost,
}

pub struct BrowseModel {
    modules: Vec<BrowseModule>,
    hosts: Vec<HostInfo>,
    list: List<DefaultItem>,
    host_list: List<DefaultItem>,
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
        let items = cfg
            .modules
            .iter()
            .map(|m| DefaultItem::new(&m.node, &m.kind))
            .collect();
        let host_items = cfg
            .hosts
            .iter()
            .map(|h| DefaultItem::new(&h.name, ""))
            .collect();
        let mut model = BrowseModel {
            modules: cfg.modules.clone(),
            hosts: cfg.hosts.clone(),
            list: widgets::styled_list(items, w, h),
            host_list: widgets::styled_list(host_items, w, h),
            mode: Mode::List,
            doc: viewport::new(w, h),
            dims: (w, h),
        };
        model.sync_doc();
        model
    }

    // ---- selection lookups (filter-safe: by the selected item's title) ----

    fn selected_module(&self) -> Option<&BrowseModule> {
        let node = self.list.selected_item()?.title.clone();
        self.modules.iter().find(|m| m.node == node)
    }

    fn selected_host(&self) -> Option<&HostInfo> {
        let name = self.host_list.selected_item()?.title.clone();
        self.hosts.iter().find(|h| h.name == name)
    }

    fn sync_doc(&mut self) {
        let text = self
            .selected_module()
            .map(|m| m.doc.clone())
            .unwrap_or_default();
        self.doc.goto_top();
        self.doc.set_content(&text);
    }

    fn resize(&mut self, size: (u16, u16)) {
        let dims = view_dims(size);
        if dims != self.dims {
            self.dims = dims;
            self.list.set_size(dims.0, dims.1);
            self.host_list.set_size(dims.0, dims.1);
            let text = self
                .selected_module()
                .map(|m| m.doc.clone())
                .unwrap_or_default();
            self.doc = viewport::new(dims.0, dims.1);
            self.doc.set_content(&text);
        }
    }

    /// The navigation intent for the focused action. In list mode `insert` opens the host
    /// picker (unless there are no modules or hosts); in pick mode it commits.
    fn activate(&mut self) -> Nav {
        match self.mode {
            Mode::List => {
                if self.modules.is_empty() || self.hosts.is_empty() {
                    return Nav::Stay;
                }
                self.mode = Mode::PickHost;
                Nav::Stay
            }
            Mode::PickHost => match (self.selected_module(), self.selected_host()) {
                (Some(m), Some(h)) => Nav::Insert {
                    host: h.clone(),
                    node: m.node.clone(),
                    skeleton: m.skeleton.clone(),
                },
                _ => Nav::Stay,
            },
        }
    }

    pub fn update(&mut self, msg: Msg, size: (u16, u16)) -> Step {
        if msg.downcast_ref::<WindowSizeMsg>().is_some() {
            self.resize(size);
            return Step::stay();
        }
        let Some((code, mods)) = key_of(&msg) else {
            return Step::stay();
        };

        if matches!(code, KeyCode::Char('c')) && mods.contains(KeyModifiers::CONTROL) {
            return Step::nav(Nav::Back);
        }

        match self.mode {
            Mode::List => match code {
                KeyCode::Esc | KeyCode::Char('q') => Step::nav(Nav::Back),
                KeyCode::Enter | KeyCode::Char('i') => Step::nav(self.activate()),
                // Only declarative modules carry a manifest to edit; a built-in (manifest
                // `None`) leaves the selection untouched rather than opening a broken editor.
                KeyCode::Char('e') => {
                    match self.selected_module().and_then(|m| m.manifest.clone()) {
                        Some(manifest) => Step::nav(Nav::EditModule { manifest }),
                        None => Step::stay(),
                    }
                }
                // PgUp/PgDn scroll the doc; everything else drives the module list.
                KeyCode::PageUp | KeyCode::PageDown => {
                    let cmd = self.doc.update(msg);
                    Step {
                        nav: Nav::Stay,
                        cmd,
                    }
                }
                _ => {
                    let cmd = self.list.update(msg);
                    self.sync_doc();
                    Step {
                        nav: Nav::Stay,
                        cmd,
                    }
                }
            },
            Mode::PickHost => match code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.mode = Mode::List;
                    Step::stay()
                }
                KeyCode::Enter => Step::nav(self.activate()),
                _ => {
                    let cmd = self.host_list.update(msg);
                    Step {
                        nav: Nav::Stay,
                        cmd,
                    }
                }
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

        // Both panes are given a fixed width and height so the boxes never resize as the
        // selection (and thus the content's max line width) changes.
        let (w, h) = (self.dims.0 as i32, self.dims.1 as i32);
        let list_box = Style::new()
            .border(rounded_border())
            .border_foreground(theme::border(true))
            .width(w)
            .height(h)
            .render(&self.list.view());
        let doc_box = Style::new()
            .border(rounded_border())
            .border_foreground(theme::border(false))
            .width(w)
            .height(h)
            .render(&self.doc.view());
        let panes = join_horizontal(TOP, &[list_box.as_str(), "  ", doc_box.as_str()]);
        let footer = if self.hosts.is_empty() {
            theme::amber().render("no hosts under hosts/ \u{2013} nothing to insert into")
        } else {
            widgets::footer(&[
                ("\u{2191}/\u{2193}", "select"),
                ("i", "insert"),
                ("e", "edit"),
                ("pgup/pgdn", "scroll"),
                ("esc", "back"),
            ])
        };
        format!("{}\n{}\n{}", theme::chip(" browse "), panes, footer)
    }

    fn view_pick(&self) -> String {
        let node = self
            .selected_module()
            .map(|m| m.node.as_str())
            .unwrap_or("module");
        let boxed = Style::new()
            .border(rounded_border())
            .border_foreground(theme::border(true))
            .render(&self.host_list.view());
        format!(
            "{}\n{}\n{}",
            theme::chip(&format!(" insert {node} into ")),
            boxed,
            widgets::footer(&[
                ("\u{2191}/\u{2193}", "pick"),
                ("enter", "insert"),
                ("esc", "cancel")
            ]),
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
        HostInfo {
            name: name.into(),
            default: false,
            path: PathBuf::from(format!("hosts/{name}.kdl")),
        }
    }

    fn module(node: &str, kind: &str) -> BrowseModule {
        // Declarative modules carry a manifest path (matching `main.rs::browse_modules`'
        // layout); built-ins carry `None`, since there is nothing on disk to edit.
        let manifest = (kind == "declarative")
            .then(|| PathBuf::from(format!("modules/{node}/knixl-module.kdl")));
        BrowseModule {
            node: node.into(),
            kind: kind.into(),
            doc: format!("# {node}\n\nthe {node} module\n"),
            skeleton: node.into(),
            manifest,
        }
    }

    fn model(mods: &[(&str, &str)], hosts: usize) -> BrowseModel {
        let modules: Vec<BrowseModule> = mods.iter().map(|(n, k)| module(n, k)).collect();
        let host_infos: Vec<HostInfo> = (0..hosts).map(|i| host(&format!("h{i}"))).collect();
        let items = modules
            .iter()
            .map(|m| DefaultItem::new(&m.node, &m.kind))
            .collect();
        let host_items = host_infos
            .iter()
            .map(|h| DefaultItem::new(&h.name, ""))
            .collect();
        let mut m = BrowseModel {
            modules,
            hosts: host_infos,
            list: widgets::styled_list(items, 40, 10),
            host_list: widgets::styled_list(host_items, 40, 10),
            mode: Mode::List,
            doc: viewport::new(40, 10),
            dims: (40, 10),
        };
        m.sync_doc();
        m
    }

    fn key(code: KeyCode) -> Msg {
        Box::new(KeyMsg {
            key: code,
            modifiers: KeyModifiers::NONE,
        }) as Msg
    }

    #[test]
    fn selecting_moves_and_updates_the_doc() {
        let mut m = model(
            &[("postgres", "built-in"), ("web-service", "declarative")],
            1,
        );
        assert!(m.doc.view().contains("postgres"));
        m.update(key(KeyCode::Down), (120, 30));
        assert_eq!(m.list.cursor(), 1);
        assert!(
            m.doc.view().contains("web-service"),
            "doc follows selection"
        );
    }

    #[test]
    fn insert_opens_the_host_picker_then_commits() {
        let mut m = model(&[("postgres", "built-in")], 2);
        assert!(matches!(
            m.update(key(KeyCode::Char('i')), (120, 30)).nav,
            Nav::Stay
        ));
        assert_eq!(m.mode, Mode::PickHost);
        m.update(key(KeyCode::Down), (120, 30)); // pick the second host
        assert_eq!(m.host_list.cursor(), 1);
        match m.update(key(KeyCode::Enter), (120, 30)).nav {
            Nav::Insert {
                host,
                node,
                skeleton,
            } => {
                assert_eq!(host.name, "h1");
                assert_eq!(node, "postgres");
                assert_eq!(skeleton, "postgres");
            }
            _ => panic!("expected an insert"),
        }
    }

    #[test]
    fn edit_action_only_for_declarative_modules() {
        let mut m = model(
            &[("postgres", "built-in"), ("web-service", "declarative")],
            1,
        );
        // The built-in is selected first; edit has no manifest to open, so it stays put.
        assert!(matches!(
            m.update(key(KeyCode::Char('e')), (120, 30)).nav,
            Nav::Stay
        ));
        m.update(key(KeyCode::Down), (120, 30)); // select the declarative module
        match m.update(key(KeyCode::Char('e')), (120, 30)).nav {
            Nav::EditModule { manifest } => {
                assert_eq!(
                    manifest,
                    PathBuf::from("modules/web-service/knixl-module.kdl")
                );
            }
            _ => panic!("expected Nav::EditModule for a declarative selection"),
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
        let m = model(
            &[("postgres", "built-in"), ("web-service", "declarative")],
            1,
        );
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
