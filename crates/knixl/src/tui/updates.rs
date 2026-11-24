//! Updates screen: runs the `knixl updates` report in the background, shows it, and lets you
//! choose what to take: the whole nixpkgs refresh (optionally only if it fixes CVEs), or
//! version pins for packages the hosts install directly. Applying ends the session and the
//! CLI carries the choice out.

use bubbletea_rs::event::{KeyMsg, WindowSizeMsg};
use bubbletea_rs::{command, Cmd, Model as BubbleTeaModel, Msg};
use bubbletea_widgets::{spinner, viewport};
use crossterm::event::{KeyCode, KeyModifiers};
use lipgloss::{join_vertical, rounded_border, Style, LEFT};

use knixl_pipeline::updates::{direct_updates, render_markdown, HostReport};

use super::{config, theme, widgets, Nav, Step};

/// The background report, delivered back to `update`.
struct UpdatesDone(Result<Vec<HostReport>, String>);

/// One choice on the screen.
#[derive(Debug, Clone, PartialEq)]
pub enum Choice {
    Refresh,
    IfSecurity,
    Pin {
        host: String,
        attr: String,
        version: String,
    },
}

enum State {
    Running,
    Ready,
    Failed(String),
}

pub struct UpdatesModel {
    state: State,
    spinner: spinner::Model,
    preview: viewport::Model,
    choices: Vec<(Choice, bool)>,
    /// Index into `choices`, then Apply, then Cancel.
    focus: usize,
    summary: Vec<String>,
}

fn new_spinner() -> spinner::Model {
    spinner::new(&[
        spinner::with_spinner(spinner::DOT.clone()),
        spinner::with_style(theme::amber()),
    ])
}

fn dims(size: (u16, u16)) -> (usize, usize) {
    (
        (size.0 as usize).saturating_sub(8).max(20),
        (size.1 as usize).saturating_sub(16).max(4),
    )
}

impl UpdatesModel {
    /// Open the screen and start the report, if the project can produce one.
    pub fn enter(size: (u16, u16)) -> (Self, Option<Cmd>) {
        let (w, h) = dims(size);
        let mut m = UpdatesModel {
            state: State::Running,
            spinner: new_spinner(),
            preview: viewport::new(w, h),
            choices: Vec::new(),
            focus: 0,
            summary: Vec::new(),
        };
        let Some(report) = config().updates.clone() else {
            m.state = State::Failed(
                "updates need a generated flake: add a system block to knixl.kdl".into(),
            );
            return (m, None);
        };
        let run: Cmd = Box::pin(async move {
            match tokio::task::spawn_blocking(move || report()).await {
                Ok(r) => Some(Box::new(UpdatesDone(r)) as Msg),
                Err(_) => None,
            }
        });
        let tick = m.spinner.tick_msg();
        let spin: Cmd = Box::pin(async move { Some(Box::new(tick) as Msg) });
        (m, Some(command::batch(vec![run, spin])))
    }

    fn on_report(&mut self, reports: Vec<HostReport>) {
        self.preview.set_content(&render_markdown(&reports));
        self.summary = reports
            .iter()
            .map(|r| {
                let fixed: usize = r
                    .security
                    .as_ref()
                    .map(|s| s.fixed.iter().map(|f| f.cves.len()).sum())
                    .unwrap_or(0);
                let security = match &r.security {
                    Some(_) => format!("{fixed} CVEs fixed"),
                    None => "security not checked".into(),
                };
                format!(
                    "{}: {} version changes, {security}",
                    r.host,
                    r.diff.changed.len()
                )
            })
            .collect();
        if !reports.is_empty() {
            self.choices.push((Choice::Refresh, false));
            self.choices.push((Choice::IfSecurity, false));
            for r in &reports {
                for (attr, version) in direct_updates(r) {
                    self.choices.push((
                        Choice::Pin {
                            host: r.host.clone(),
                            attr,
                            version,
                        },
                        false,
                    ));
                }
            }
        }
        self.state = State::Ready;
    }

    fn is_on(&self, c: &Choice) -> bool {
        self.choices.iter().any(|(x, on)| x == c && *on)
    }

    /// The pins are part of the full refresh, so they only count when it isn't chosen.
    fn apply_nav(&self) -> Nav {
        let refresh = self.is_on(&Choice::Refresh);
        Nav::ApplyUpdates {
            refresh,
            if_security: refresh && self.is_on(&Choice::IfSecurity),
            pins: if refresh {
                Vec::new()
            } else {
                self.choices
                    .iter()
                    .filter(|(_, on)| *on)
                    .filter_map(|(c, _)| match c {
                        Choice::Pin {
                            host,
                            attr,
                            version,
                        } => Some((host.clone(), attr.clone(), version.clone())),
                        _ => None,
                    })
                    .collect()
            },
        }
    }

    fn anything_chosen(&self) -> bool {
        self.choices
            .iter()
            .any(|(c, on)| *on && *c != Choice::IfSecurity)
    }

    pub fn update(&mut self, msg: Msg, size: (u16, u16)) -> Step {
        if msg.downcast_ref::<WindowSizeMsg>().is_some() {
            let (w, h) = dims(size);
            self.preview.width = w;
            self.preview.height = h;
            return Step::stay();
        }
        let msg = match msg.downcast::<UpdatesDone>() {
            Ok(done) => {
                match done.0 {
                    Ok(reports) => self.on_report(reports),
                    Err(e) => self.state = State::Failed(e),
                }
                return Step::stay();
            }
            Err(msg) => msg,
        };
        if msg.downcast_ref::<spinner::TickMsg>().is_some() {
            let cmd = self.spinner.update(msg);
            return Step {
                nav: Nav::Stay,
                cmd: matches!(self.state, State::Running)
                    .then_some(cmd)
                    .flatten(),
            };
        }
        let Some(key) = msg.downcast_ref::<KeyMsg>() else {
            return Step::stay();
        };
        let (code, mods) = (key.key, key.modifiers);
        if code == KeyCode::Esc
            || (code == KeyCode::Char('c') && mods.contains(KeyModifiers::CONTROL))
        {
            return Step::nav(Nav::Back);
        }
        if !matches!(self.state, State::Ready) {
            return Step::stay();
        }
        let slots = self.choices.len() + 2;
        match code {
            KeyCode::Tab | KeyCode::Down => self.focus = (self.focus + 1) % slots,
            KeyCode::BackTab | KeyCode::Up => self.focus = (self.focus + slots - 1) % slots,
            KeyCode::PageUp | KeyCode::PageDown => {
                let cmd = self.preview.update(msg);
                return Step {
                    nav: Nav::Stay,
                    cmd,
                };
            }
            KeyCode::Char(' ') | KeyCode::Enter if self.focus < self.choices.len() => {
                self.choices[self.focus].1 = !self.choices[self.focus].1;
            }
            KeyCode::Enter if self.focus == self.choices.len() && self.anything_chosen() => {
                return Step::nav(self.apply_nav());
            }
            KeyCode::Enter if self.focus == self.choices.len() + 1 => {
                return Step::nav(Nav::Back);
            }
            _ => {}
        }
        Step::stay()
    }

    fn label(&self, c: &Choice) -> String {
        let refresh = self.is_on(&Choice::Refresh);
        match c {
            Choice::Refresh => "take the whole nixpkgs update".into(),
            Choice::IfSecurity => "  ...only if it fixes CVEs".into(),
            Choice::Pin {
                host,
                attr,
                version,
            } => {
                let text = format!("pin {attr} to {version} on {host}");
                if refresh {
                    theme::dim().render(&format!("{text} (part of the update)"))
                } else {
                    text
                }
            }
        }
    }

    pub fn view(&self, _size: (u16, u16)) -> String {
        let mut lines = vec![theme::chip(" updates ")];
        match &self.state {
            State::Running => lines.push(format!(
                "{} checking for updates (this takes a few minutes)",
                self.spinner.view()
            )),
            State::Failed(e) => lines.push(theme::bad().render(&format!("\u{2717} {e}"))),
            State::Ready if self.choices.is_empty() => lines.push(
                theme::good().render("\u{2713} every host is on the newest commit of its release"),
            ),
            State::Ready => {
                for s in &self.summary {
                    lines.push(s.clone());
                }
                lines.push(String::new());
                // One left-aligned block, so the toggles line up when the screen is centred.
                let mut block = Vec::new();
                for (i, (c, on)) in self.choices.iter().enumerate() {
                    let marker = if self.focus == i {
                        theme::accent().render("\u{25b8} ")
                    } else {
                        "  ".into()
                    };
                    block.push(format!("{marker}{} {}", theme::toggle(*on), self.label(c)));
                }
                let button = |label: &str, focused: bool, enabled: bool| {
                    let text = format!(" {label} ");
                    if focused {
                        theme::selected().render(&text)
                    } else if enabled {
                        theme::accent().render(&text)
                    } else {
                        theme::dim().render(&text)
                    }
                };
                block.push(format!(
                    "  {}  {}",
                    button(
                        "apply",
                        self.focus == self.choices.len(),
                        self.anything_chosen()
                    ),
                    button("cancel", self.focus == self.choices.len() + 1, true)
                ));
                let refs: Vec<&str> = block.iter().map(String::as_str).collect();
                lines.push(join_vertical(LEFT, &refs));
                lines.push(
                    Style::new()
                        .border(rounded_border())
                        .border_foreground(theme::border(false))
                        .render(&self.preview.view()),
                );
            }
        }
        lines.push(widgets::footer(&[
            ("tab", "move"),
            ("space", "choose"),
            ("pgup/pgdn", "report"),
            ("esc", "back"),
        ]));
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use knixl_pipeline::updates::{Bump, ClosureDiff, VersionChange};

    fn model() -> UpdatesModel {
        let mut m = UpdatesModel {
            state: State::Running,
            spinner: new_spinner(),
            preview: viewport::new(40, 5),
            choices: Vec::new(),
            focus: 0,
            summary: Vec::new(),
        };
        m.on_report(vec![HostReport {
            host: "fw13".into(),
            release: "25.11".into(),
            old_rev: "a".repeat(40),
            new_rev: "b".repeat(40),
            diff: ClosureDiff {
                changed: vec![VersionChange {
                    pname: "curl".into(),
                    old: vec!["8.19.0".into()],
                    new: vec!["8.20.0".into()],
                    bump: Bump::Minor,
                }],
                ..Default::default()
            },
            security: None,
            direct: vec!["curl".into()],
        }]);
        m
    }

    fn key(code: KeyCode) -> Msg {
        Box::new(KeyMsg {
            key: code,
            modifiers: KeyModifiers::NONE,
        }) as Msg
    }

    #[test]
    fn choices_render_as_one_left_aligned_block() {
        let v = model().view((120, 40));
        let strip = |l: &str| {
            let mut out = String::new();
            let mut esc = false;
            for c in l.chars() {
                match (esc, c) {
                    (false, '\u{1b}') => esc = true,
                    (true, 'm') => esc = false,
                    (true, _) => {}
                    (false, c) => out.push(c),
                }
            }
            out
        };
        let rows: Vec<String> = v
            .lines()
            .map(strip)
            .filter(|l| l.contains("take the whole") || l.contains("pin curl"))
            .collect();
        assert_eq!(rows.len(), 2, "{v}");
        let toggle_at = |l: &str| l.chars().position(|c| c == '(');
        assert_eq!(toggle_at(&rows[0]), toggle_at(&rows[1]), "{rows:?}");
    }

    #[test]
    fn offers_the_refresh_and_a_pin_per_direct_update() {
        let m = model();
        assert_eq!(m.choices.len(), 3);
        assert!(
            matches!(&m.choices[2].0, Choice::Pin { attr, version, .. } if attr == "curl" && version == "8.20.0")
        );
        assert!(!m.anything_chosen(), "apply starts off");
    }

    #[test]
    fn choosing_a_pin_applies_just_that_pin() {
        let mut m = model();
        m.focus = 2;
        m.update(key(KeyCode::Char(' ')), (80, 24));
        m.focus = 3; // apply
        match m.update(key(KeyCode::Enter), (80, 24)).nav {
            Nav::ApplyUpdates { refresh, pins, .. } => {
                assert!(!refresh);
                assert_eq!(pins, vec![("fw13".into(), "curl".into(), "8.20.0".into())]);
            }
            _ => panic!("expected apply"),
        }
    }

    #[test]
    fn the_refresh_supersedes_pins() {
        let mut m = model();
        for i in [0, 1, 2] {
            m.focus = i;
            m.update(key(KeyCode::Char(' ')), (80, 24));
        }
        m.focus = 3;
        match m.update(key(KeyCode::Enter), (80, 24)).nav {
            Nav::ApplyUpdates {
                refresh,
                if_security,
                pins,
            } => {
                assert!(refresh && if_security);
                assert!(pins.is_empty());
            }
            _ => panic!("expected apply"),
        }
    }
}
