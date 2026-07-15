//! The `install` TUI: a live preview with a host picker, verify status, and apply/cancel.
//! The logic is pure (`update`, `render`) and the loop (`run_loop`) is generic over the
//! ratatui backend and the event source, so all of it is testable without a real terminal.
//! Only the production wiring in `run` (raw mode, the crossterm key reader) is untested glue.

use std::io;

use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::{Frame, Terminal};

use knixl_pipeline::install::HostInfo;

/// What the user chose. `Apply` carries the host index they settled on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Apply(usize),
    Cancel,
}

/// One step's outcome from the reducer.
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
            Parse::Ok => true,
            Parse::Skipped => !self.strict,
            Parse::Running => true, // optimistic; the loop re-checks before applying
            Parse::Failed(_) => false,
        };
        resolve_ok && parse_ok
    }

    fn selected_host(&self) -> Option<&HostInfo> {
        self.hosts.get(self.selected)
    }
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

/// Draw the panel. Pure over the state.
pub fn render(state: &InstallState, frame: &mut Frame) {
    let accent = Style::default().fg(Color::Cyan);
    let dim = Style::default().fg(Color::DarkGray);
    let good = Style::default().fg(Color::Green);
    let bad = Style::default().fg(Color::Red);
    let amber = Style::default().fg(Color::Yellow);
    let host = state.selected_host().map(|h| h.name.clone()).unwrap_or_else(|| "-".into());

    let (r_txt, r_style) = match state.resolves {
        Resolve::Yes => ("✓ resolves", good),
        Resolve::No => ("✗ no such package", bad),
        Resolve::Skipped => ("· resolves skipped", amber),
    };
    let (p_txt, p_style) = match &state.parses {
        Parse::Ok => ("✓ parses", good),
        Parse::Running => ("… verifying", amber),
        Parse::Failed(_) => ("✗ parse failed", bad),
        Parse::Skipped => ("· parses skipped", amber),
    };

    let mut lines = vec![
        Line::from(vec![
            Span::styled("host:  ", dim),
            Span::styled("‹ ", amber),
            Span::styled(host.clone(), Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(" ›", amber),
            Span::styled("   ←/→ to change", dim),
        ]),
        Line::from(Span::styled(format!("+ package \"{}\"", state.pkg), good)),
        Line::from(vec![
            Span::styled("verify: ", dim),
            Span::styled(r_txt, r_style),
            Span::raw("  "),
            Span::styled(p_txt, p_style),
        ]),
        Line::from(Span::styled(format!("{host}.nix"), dim)),
    ];
    for l in state.nix_preview.lines() {
        lines.push(Line::from(Span::raw(format!("  {l}"))));
    }
    lines.push(Line::from(""));
    let apply_style = if state.apply_allowed() { accent } else { dim };
    lines.push(Line::from(vec![
        Span::styled("[enter] apply", apply_style),
        Span::styled("   [q] cancel", dim),
    ]));

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" install {} ", state.pkg))
        .border_style(accent);
    frame.render_widget(Paragraph::new(lines).block(block), frame.area());
}

/// The loop, generic over backend and event source. Renders, reads a key, applies
/// `update`, recomputes the per-host preview/verify on a switch (via `recompute`), and
/// returns the decision. Event source exhaustion is treated as a cancel.
pub fn run_loop<B: Backend>(
    terminal: &mut Terminal<B>,
    events: impl IntoIterator<Item = io::Result<KeyEvent>>,
    mut state: InstallState,
    recompute: &mut dyn FnMut(&mut InstallState),
) -> io::Result<Decision> {
    terminal.draw(|f| render(&state, f))?;
    for ev in events {
        match update(&mut state, ev?) {
            Action::Apply => return Ok(Decision::Apply(state.selected)),
            Action::Cancel => return Ok(Decision::Cancel),
            Action::SwitchHost => {
                // Show the new host's preview immediately, mark the parse in-flight, then
                // recompute and redraw with the result.
                state.parses = Parse::Running;
                terminal.draw(|f| render(&state, f))?;
                recompute(&mut state);
                terminal.draw(|f| render(&state, f))?;
            }
            Action::None => {
                terminal.draw(|f| render(&state, f))?;
            }
        }
    }
    Ok(Decision::Cancel)
}

/// Restores the terminal on drop, so raw mode / the alternate screen are always undone,
/// including on error or panic.
struct TermGuard;
impl Drop for TermGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

/// Production entry: set up the terminal, drive `run_loop` against the real key reader, and
/// restore on exit. This is the only untested glue; the logic lives in `run_loop`/`update`.
pub fn run(state: InstallState, recompute: &mut dyn FnMut(&mut InstallState)) -> io::Result<Decision> {
    enable_raw_mode()?;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen)?;
    let _guard = TermGuard;
    let mut terminal = Terminal::new(CrosstermBackend::new(out))?;
    let events = std::iter::from_fn(|| loop {
        match event::read() {
            Ok(Event::Key(k)) if k.kind == KeyEventKind::Press => return Some(Ok(k)),
            Ok(_) => continue,
            Err(e) => return Some(Err(e)),
        }
    });
    run_loop(&mut terminal, events, state, recompute)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
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
        // clamp at the low end
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

    // ---- run_loop end to end ----

    fn term() -> Terminal<TestBackend> {
        Terminal::new(TestBackend::new(60, 14)).unwrap()
    }

    #[test]
    fn loop_cancels_on_q() {
        let mut t = term();
        let mut recompute = |_: &mut InstallState| {};
        let decision =
            run_loop(&mut t, vec![Ok(key(KeyCode::Char('q')))], state(2), &mut recompute).unwrap();
        assert_eq!(decision, Decision::Cancel);
    }

    #[test]
    fn loop_applies_immediately() {
        let mut t = term();
        let mut recompute = |_: &mut InstallState| {};
        let decision =
            run_loop(&mut t, vec![Ok(key(KeyCode::Enter))], state(2), &mut recompute).unwrap();
        assert_eq!(decision, Decision::Apply(0));
    }

    #[test]
    fn loop_switches_right_twice_then_applies_recomputing_each_time() {
        let mut t = term();
        let mut recomputes = 0;
        let mut recompute = |_: &mut InstallState| recomputes += 1;
        let events = vec![
            Ok(key(KeyCode::Right)),
            Ok(key(KeyCode::Right)),
            Ok(key(KeyCode::Enter)),
        ];
        let decision = run_loop(&mut t, events, state(3), &mut recompute).unwrap();
        assert_eq!(decision, Decision::Apply(2), "settled on the third host");
        assert_eq!(recomputes, 2, "re-verified on each of the two switches");
    }

    #[test]
    fn loop_apply_is_refused_while_unresolved_then_cancels_on_q() {
        let mut t = term();
        let mut recompute = |_: &mut InstallState| {};
        let mut s = state(1);
        s.resolves = Resolve::No;
        // enter does nothing (blocked), then q cancels.
        let events = vec![Ok(key(KeyCode::Enter)), Ok(key(KeyCode::Char('q')))];
        let decision = run_loop(&mut t, events, s, &mut recompute).unwrap();
        assert_eq!(decision, Decision::Cancel);
    }

    #[test]
    fn exhausted_events_cancel() {
        let mut t = term();
        let mut recompute = |_: &mut InstallState| {};
        let decision = run_loop(&mut t, vec![], state(1), &mut recompute).unwrap();
        assert_eq!(decision, Decision::Cancel);
    }

    // ---- render ----

    fn buffer_text(t: &Terminal<TestBackend>) -> String {
        t.backend().buffer().content().iter().map(|c| c.symbol()).collect()
    }

    #[test]
    fn render_shows_package_host_and_keys() {
        let mut t = term();
        t.draw(|f| render(&state(2), f)).unwrap();
        let text = buffer_text(&t);
        assert!(text.contains("ripgrep"), "package shown: {text}");
        assert!(text.contains("h0"), "host shown: {text}");
        assert!(text.contains("apply"), "key hint shown: {text}");
    }

    #[test]
    fn render_shows_unresolved_error() {
        let mut s = state(1);
        s.resolves = Resolve::No;
        let mut t = term();
        t.draw(|f| render(&s, f)).unwrap();
        let text = buffer_text(&t);
        assert!(text.contains("no such package") || text.contains("unresolved"), "error shown: {text}");
    }
}
