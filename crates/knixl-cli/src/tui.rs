//! The `install` TUI: a lipgloss-styled preview with a host picker, verify status, and
//! apply/cancel. `render` is a pure `&state -> String`, `update` is a pure reducer, and
//! `run_loop` is generic over the output sink and the event source, so all of it is
//! testable without a real terminal. Only the raw-mode/key-reader glue in `run` is untested.

use std::io::{self, Write};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use lipgloss::{join_vertical, rounded_border, Color, Style, LEFT};

use knixl_pipeline::install::HostInfo;

/// What the user chose. `Apply` carries the host index they settled on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Apply(usize),
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Apply,
    Cancel,
    SwitchHost,
    None,
}

/// Does `pkgs.<pkg>` resolve (host-independent, computed once).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolve {
    Yes,
    No,
    Skipped,
}

/// Does the drafted file parse (per host, re-run on a switch).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parse {
    Running,
    Ok,
    Failed(String),
    Skipped,
}

pub struct InstallState {
    pub pkg: String,
    pub hosts: Vec<HostInfo>,
    pub selected: usize,
    pub strict: bool,
    pub resolves: Resolve,
    pub parses: Parse,
    pub nix_preview: String,
}

impl InstallState {
    /// Apply is allowed when the package resolves (or was skipped without `--strict`) and
    /// the parse did not fail (and was not skipped under `--strict`).
    pub fn apply_allowed(&self) -> bool {
        if self.hosts.is_empty() {
            return false;
        }
        let resolve_ok = match self.resolves {
            Resolve::Yes => true,
            Resolve::Skipped => !self.strict,
            Resolve::No => false,
        };
        let parse_ok = match &self.parses {
            Parse::Ok | Parse::Running => true,
            Parse::Skipped => !self.strict,
            Parse::Failed(_) => false,
        };
        resolve_ok && parse_ok
    }

    fn selected_host(&self) -> Option<&HostInfo> {
        self.hosts.get(self.selected)
    }
}

fn color(code: &str) -> Color {
    Color(code.to_string())
}

/// Draw the panel to a styled string (with ANSI). Pure over the state.
pub fn render(state: &InstallState) -> String {
    let dim = Style::new().foreground(color("8"));
    let good = Style::new().foreground(color("2"));
    let bad = Style::new().foreground(color("1"));
    let amber = Style::new().foreground(color("3"));
    let accent = Style::new().foreground(color("6"));
    let bold = Style::new().bold(true);

    let host = state.selected_host().map(|h| h.name.clone()).unwrap_or_else(|| "-".into());

    let (r_txt, r_style) = match state.resolves {
        Resolve::Yes => ("✓ resolves", &good),
        Resolve::No => ("✗ no such package", &bad),
        Resolve::Skipped => ("· resolves skipped", &amber),
    };
    let (p_txt, p_style) = match &state.parses {
        Parse::Ok => ("✓ parses", &good),
        Parse::Running => ("… verifying", &amber),
        Parse::Failed(_) => ("✗ parse failed", &bad),
        Parse::Skipped => ("· parses skipped", &amber),
    };

    let mut lines = vec![
        format!(
            "{}{}{}{}{}",
            dim.render("host:  "),
            amber.render("‹ "),
            bold.render(&host),
            amber.render(" ›"),
            dim.render("   ←/→ to change"),
        ),
        good.render(&format!("+ package \"{}\"", state.pkg)),
        format!("{}{}  {}", dim.render("verify: "), r_style.render(r_txt), p_style.render(p_txt)),
        dim.render(&format!("{host}.nix")),
    ];
    for l in state.nix_preview.lines() {
        lines.push(format!("  {l}"));
    }
    lines.push(String::new());
    let apply = "[enter] apply";
    let apply = if state.apply_allowed() { accent.render(apply) } else { dim.render(apply) };
    lines.push(format!("{}{}", apply, dim.render("   [q] cancel")));

    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    let body = join_vertical(LEFT, &refs);

    let panel =
        Style::new().border(rounded_border()).border_foreground(color("6")).padding_2(0, 1);
    format!("{}\n{}", accent.render(&format!(" install {} ", state.pkg)), panel.render(&body))
}

/// Pure reducer: fold a key press into the state and report what the loop should do.
fn update(state: &mut InstallState, key: KeyEvent) -> Action {
    match key.code {
        KeyCode::Left => {
            if state.selected > 0 {
                state.selected -= 1;
                Action::SwitchHost
            } else {
                Action::None
            }
        }
        KeyCode::Right => {
            if state.selected + 1 < state.hosts.len() {
                state.selected += 1;
                Action::SwitchHost
            } else {
                Action::None
            }
        }
        KeyCode::Enter => {
            if state.apply_allowed() {
                Action::Apply
            } else {
                Action::None
            }
        }
        KeyCode::Esc | KeyCode::Char('q') => Action::Cancel,
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => Action::Cancel,
        _ => Action::None,
    }
}

/// Clear the screen and write the current frame (raw mode needs CRLF line endings).
fn draw<W: Write>(out: &mut W, state: &InstallState) -> io::Result<()> {
    let frame = render(state).replace('\n', "\r\n");
    write!(out, "\x1b[2J\x1b[H{frame}")?;
    out.flush()
}

/// The loop, generic over the output sink and the event source. Renders, reads a key,
/// applies `update`, recomputes the per-host preview on a switch, and returns the decision.
/// Event-source exhaustion is treated as a cancel.
pub fn run_loop<W: Write>(
    out: &mut W,
    events: impl IntoIterator<Item = io::Result<KeyEvent>>,
    mut state: InstallState,
    recompute: &mut dyn FnMut(&mut InstallState),
) -> io::Result<Decision> {
    draw(out, &state)?;
    for ev in events {
        match update(&mut state, ev?) {
            Action::Apply => return Ok(Decision::Apply(state.selected)),
            Action::Cancel => return Ok(Decision::Cancel),
            Action::SwitchHost => {
                state.parses = Parse::Running;
                draw(out, &state)?;
                recompute(&mut state);
                draw(out, &state)?;
            }
            Action::None => draw(out, &state)?,
        }
    }
    Ok(Decision::Cancel)
}

/// Restores the terminal on drop, so raw mode / the alternate screen are always undone.
struct TermGuard;
impl Drop for TermGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

/// Production entry: set up the terminal, drive `run_loop` against the real key reader, and
/// restore on exit. The only untested glue; the logic lives in `run_loop`/`update`/`render`.
pub fn run(state: InstallState, recompute: &mut dyn FnMut(&mut InstallState)) -> io::Result<Decision> {
    enable_raw_mode()?;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen)?;
    let _guard = TermGuard;
    let events = std::iter::from_fn(|| loop {
        match event::read() {
            Ok(Event::Key(k)) if k.kind == KeyEventKind::Press => return Some(Ok(k)),
            Ok(_) => continue,
            Err(e) => return Some(Err(e)),
        }
    });
    run_loop(&mut out, events, state, recompute)
}



#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn host(name: &str) -> HostInfo {
        HostInfo { name: name.into(), default: false, path: PathBuf::from(format!("hosts/{name}.kdl")) }
    }

    fn state(hosts: usize) -> InstallState {
        InstallState {
            pkg: "ripgrep".into(),
            hosts: (0..hosts).map(|i| host(&format!("h{i}"))).collect(),
            selected: 0,
            strict: false,
            resolves: Resolve::Yes,
            parses: Parse::Ok,
            nix_preview: "environment.systemPackages = [ pkgs.ripgrep ];".into(),
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    // ---- update reducer ----

    #[test]
    fn right_and_left_move_the_selection_clamped() {
        let mut s = state(3);
        assert_eq!(update(&mut s, key(KeyCode::Right)), Action::SwitchHost);
        assert_eq!(s.selected, 1);
        assert_eq!(update(&mut s, key(KeyCode::Left)), Action::SwitchHost);
        assert_eq!(s.selected, 0);
        assert_eq!(update(&mut s, key(KeyCode::Left)), Action::None);
        assert_eq!(s.selected, 0);
    }

    #[test]
    fn right_clamps_at_the_last_host() {
        let mut s = state(2);
        s.selected = 1;
        assert_eq!(update(&mut s, key(KeyCode::Right)), Action::None);
        assert_eq!(s.selected, 1);
    }

    #[test]
    fn enter_applies_when_allowed() {
        let mut s = state(1);
        assert_eq!(update(&mut s, key(KeyCode::Enter)), Action::Apply);
    }

    #[test]
    fn enter_is_blocked_when_unresolved() {
        let mut s = state(1);
        s.resolves = Resolve::No;
        assert_eq!(update(&mut s, key(KeyCode::Enter)), Action::None);
    }

    #[test]
    fn q_and_esc_and_ctrl_c_cancel() {
        let mut s = state(1);
        assert_eq!(update(&mut s, key(KeyCode::Char('q'))), Action::Cancel);
        assert_eq!(update(&mut s, key(KeyCode::Esc)), Action::Cancel);
        assert_eq!(
            update(&mut s, KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Action::Cancel
        );
    }

    // ---- run_loop end to end (over a Vec<u8> sink) ----

    #[test]
    fn loop_cancels_on_q() {
        let mut out = Vec::new();
        let mut recompute = |_: &mut InstallState| {};
        let d = run_loop(&mut out, vec![Ok(key(KeyCode::Char('q')))], state(2), &mut recompute).unwrap();
        assert_eq!(d, Decision::Cancel);
    }

    #[test]
    fn loop_applies_immediately() {
        let mut out = Vec::new();
        let mut recompute = |_: &mut InstallState| {};
        let d = run_loop(&mut out, vec![Ok(key(KeyCode::Enter))], state(2), &mut recompute).unwrap();
        assert_eq!(d, Decision::Apply(0));
    }

    #[test]
    fn loop_switches_right_twice_then_applies_recomputing_each_time() {
        let mut out = Vec::new();
        let mut recomputes = 0;
        let mut recompute = |_: &mut InstallState| recomputes += 1;
        let events =
            vec![Ok(key(KeyCode::Right)), Ok(key(KeyCode::Right)), Ok(key(KeyCode::Enter))];
        let d = run_loop(&mut out, events, state(3), &mut recompute).unwrap();
        assert_eq!(d, Decision::Apply(2), "settled on the third host");
        assert_eq!(recomputes, 2, "re-verified on each of the two switches");
    }

    #[test]
    fn loop_apply_is_refused_while_unresolved_then_cancels_on_q() {
        let mut out = Vec::new();
        let mut recompute = |_: &mut InstallState| {};
        let mut s = state(1);
        s.resolves = Resolve::No;
        let events = vec![Ok(key(KeyCode::Enter)), Ok(key(KeyCode::Char('q')))];
        let d = run_loop(&mut out, events, s, &mut recompute).unwrap();
        assert_eq!(d, Decision::Cancel);
    }

    #[test]
    fn exhausted_events_cancel() {
        let mut out = Vec::new();
        let mut recompute = |_: &mut InstallState| {};
        let d = run_loop(&mut out, vec![], state(1), &mut recompute).unwrap();
        assert_eq!(d, Decision::Cancel);
    }

    // ---- render (asserts on the styled string) ----

    #[test]
    fn render_shows_package_host_and_keys() {
        let s = render(&state(2));
        assert!(s.contains("ripgrep"), "package shown");
        assert!(s.contains("h0"), "host shown");
        assert!(s.contains("apply"), "key hint shown");
    }

    #[test]
    fn render_shows_unresolved_error() {
        let mut s = state(1);
        s.resolves = Resolve::No;
        let out = render(&s);
        assert!(out.contains("no such package"), "error shown");
    }
}

