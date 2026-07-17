//! Install screen: pick a host, edit the package name, toggle `--strict`, watch the nix
//! verify run (async, with a spinner), scroll the generated `.nix`, and Apply or Cancel.
//!
//! The reducer is split so the decision logic (focus movement, host switch, apply gating,
//! verify-result handling) is pure and unit-tested, while the parts that spawn async `Cmd`s
//! (the nix verify, the spinner tick) read the injected `config()` and stay as thin glue.

use bubbletea_rs::event::{KeyMsg, WindowSizeMsg};
use bubbletea_rs::{command, Cmd, Model as BubbleTeaModel, Msg};
use bubbletea_widgets::{spinner, textinput, viewport};
use crossterm::event::{KeyCode, KeyModifiers};
use lipgloss::{join_vertical, rounded_border, Style, LEFT};

use knixl_pipeline::install::HostInfo;

use super::{config, theme, widgets, BuildOutcome, Entry, Nav, PinOutcome, Step, Verified};

/// Does `pkgs.<pkg>` resolve. Host-independent, recomputed when the package changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolve {
    Yes,
    No,
    Skipped,
}

/// Does the drafted host file parse under nix. Re-run per host and per package edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parse {
    Running,
    Ok,
    Failed(String),
    Skipped,
}

/// The async verify result, delivered back to `update` as a message.
struct VerifyDone {
    seq: u64,
    verified: Verified,
}

/// The `--build` status. `Off` means `--build` was not requested (never gates apply).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BuildState {
    Off,
    Building,
    Ok,
    Failed,
    Skipped,
}

/// The async build result, delivered back to `update`.
struct BuildDone {
    seq: u64,
    outcome: BuildOutcome,
}

/// The version-pin resolve status. `Off` means no version was requested (never gates apply,
/// unlike the build/verify checks, which stay strict-gated: an unresolved pin always refuses).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PinState {
    Off,
    Resolving,
    Resolved,
    Failed,
}

/// The async pin-resolve result, delivered back to `update`.
struct PinDone {
    seq: u64,
    outcome: PinOutcome,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Focus {
    Host,
    Package,
    Strict,
    Preview,
    Apply,
    Cancel,
}

const FOCUS_ORDER: [Focus; 6] =
    [Focus::Host, Focus::Package, Focus::Strict, Focus::Preview, Focus::Apply, Focus::Cancel];

pub struct InstallModel {
    pkg: textinput::Model,
    hosts: Vec<HostInfo>,
    host_sel: usize,
    strict: bool,
    /// Threaded from the entry point (`--no-abi-check`), carried unchanged through to
    /// `Nav::Apply` for the CLI to pass on to strategy selection at commit time. Never
    /// toggled from within the screen: there is no UI control for it.
    no_abi_check: bool,
    focus: Focus,
    resolves: Resolve,
    parses: Parse,
    preview: viewport::Model,
    preview_text: String,
    verifying: bool,
    seq: u64,
    spinner: spinner::Model,
    build: BuildState,
    build_spinner: spinner::Model,
    build_seq: u64,
    /// The version requested at entry (`install pkg@version`), if any. Host-independent.
    version: Option<String>,
    pin: PinState,
    pin_spinner: spinner::Model,
    pin_seq: u64,
    /// The resolved rev, set once `pin` reaches `Resolved`. Carried through to `Nav::Apply`
    /// for the CLI to write the pin.
    pin_resolved: Option<String>,
    dims: (usize, usize),
}

/// An amber dot spinner for the in-flight verify.
fn new_spinner() -> spinner::Model {
    spinner::new(&[
        spinner::with_spinner(spinner::DOT.clone()),
        spinner::with_style(theme::amber()),
    ])
}

/// Viewport dimensions for a given terminal size: a padded inner box, clamped.
fn view_dims(size: (u16, u16)) -> (usize, usize) {
    let w = (size.0 as usize).saturating_sub(6).clamp(20, 80);
    let h = (size.1 as usize).saturating_sub(12).clamp(3, 20);
    (w, h)
}

/// Below this the layout can't fit; `view` shows a resize hint instead.
fn too_small(size: (u16, u16)) -> bool {
    size.0 < 30 || size.1 < 12
}

impl InstallModel {
    /// Build the screen for the current entry, focus the package field, and kick the first
    /// verify. Reads `config()` (hosts, entry, verify), so it runs under the program only.
    pub fn enter(size: (u16, u16)) -> (Self, Option<Cmd>) {
        let cfg = config();
        let (host_sel, pkg_value, strict, version, no_abi_check) = match &cfg.entry {
            Entry::Install { pkg, strict, host, version, no_abi_check } => {
                let idx = host
                    .as_ref()
                    .and_then(|n| cfg.hosts.iter().position(|h| &h.name == n))
                    .unwrap_or(0);
                (idx, pkg.clone(), *strict, version.clone(), *no_abi_check)
            }
            Entry::Hub => (0, String::new(), false, None, false),
        };

        let mut pkg = textinput::new();
        pkg.set_placeholder("package name");
        pkg.set_value(&pkg_value);
        // focus() returns a cursor-blink command we don't drive; drop it explicitly.
        std::mem::drop(pkg.focus());

        let (w, h) = view_dims(size);
        // The injected build/pin fns are the single source of truth for whether `--build` /
        // a version were requested (Hub never injects either, so it always seeds `Off` there).
        let build_state = if cfg.build.is_some() { BuildState::Building } else { BuildState::Off };
        let pin_state =
            if cfg.pin.is_some() && version.is_some() { PinState::Resolving } else { PinState::Off };
        let mut model = InstallModel {
            pkg,
            hosts: cfg.hosts.clone(),
            host_sel,
            strict,
            no_abi_check,
            focus: Focus::Package,
            resolves: Resolve::Skipped,
            parses: Parse::Skipped,
            preview: viewport::new(w, h),
            preview_text: String::new(),
            verifying: false,
            seq: 0,
            spinner: new_spinner(),
            build: build_state,
            build_spinner: new_spinner(),
            build_seq: 0,
            version,
            pin: pin_state,
            pin_spinner: new_spinner(),
            pin_seq: 0,
            pin_resolved: None,
            dims: (w, h),
        };
        let verify = model.begin_verify();
        let build = model.begin_build();
        let pin = model.begin_pin();
        (model, batch_all(vec![verify, build, pin]))
    }

    // ---- pure decision logic (unit-tested) ----

    /// Apply is allowed only when no verify is in flight, the package resolves (or the check
    /// was skipped without `--strict`), and the parse did not fail (nor was skipped under
    /// `--strict`).
    fn apply_allowed(&self) -> bool {
        if self.hosts.is_empty() || self.verifying {
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
            Parse::Running => false,
            Parse::Failed(_) => false,
        };
        let build_ok = match self.build {
            BuildState::Off | BuildState::Ok => true,
            BuildState::Skipped => !self.strict,
            BuildState::Building | BuildState::Failed => false,
        };
        // Unlike build/verify, a requested pin is never skippable under --strict: an
        // unresolved version always refuses (docs on `install`'s pin resolution).
        let pin_ok = match self.pin {
            PinState::Off | PinState::Resolved => true,
            PinState::Resolving | PinState::Failed => false,
        };
        resolve_ok && parse_ok && build_ok && pin_ok
    }

    fn focus_index(&self) -> usize {
        FOCUS_ORDER.iter().position(|f| *f == self.focus).unwrap_or(0)
    }

    fn focus_next(&mut self) {
        let i = (self.focus_index() + 1) % FOCUS_ORDER.len();
        self.focus = FOCUS_ORDER[i];
    }

    fn focus_prev(&mut self) {
        let i = (self.focus_index() + FOCUS_ORDER.len() - 1) % FOCUS_ORDER.len();
        self.focus = FOCUS_ORDER[i];
    }

    /// Move the host selection; reports whether it actually changed (so the caller re-verifies).
    fn set_host(&mut self, idx: usize) -> bool {
        if idx < self.hosts.len() && idx != self.host_sel {
            self.host_sel = idx;
            true
        } else {
            false
        }
    }

    fn toggle_strict(&mut self) {
        self.strict = !self.strict;
    }

    /// Fold an async verify result into the state, ignoring stale results from a superseded
    /// verify (host switched or package edited again before this one returned).
    fn on_verify_done(&mut self, seq: u64, verified: Verified) {
        if seq != self.seq {
            return;
        }
        self.verifying = false;
        self.resolves = verified.resolves;
        self.parses = verified.parses;
        self.set_preview(verified.preview);
    }

    fn set_preview(&mut self, text: String) {
        self.preview.set_content(&text);
        self.preview_text = text;
    }

    /// Mark a new verify as started and return its sequence token.
    fn mark_verifying(&mut self) -> u64 {
        self.seq += 1;
        self.verifying = true;
        self.parses = Parse::Running;
        self.seq
    }

    /// Mark a new build as started and return its sequence token.
    fn mark_building(&mut self) -> u64 {
        self.build_seq += 1;
        self.build = BuildState::Building;
        self.build_seq
    }

    /// Fold an async build result into the state, ignoring a stale result from a superseded
    /// build (package edited again before this one returned).
    fn on_build_done(&mut self, seq: u64, outcome: BuildOutcome) {
        if seq != self.build_seq {
            return;
        }
        self.build = match outcome {
            BuildOutcome::Ok => BuildState::Ok,
            BuildOutcome::Failed => BuildState::Failed,
            BuildOutcome::Skipped => BuildState::Skipped,
        };
    }

    /// Mark a new pin resolve as started and return its sequence token.
    fn mark_resolving(&mut self) -> u64 {
        self.pin_seq += 1;
        self.pin = PinState::Resolving;
        self.pin_seq
    }

    /// Fold an async pin-resolve result into the state, ignoring a stale result from a
    /// superseded resolve (package edited again before this one returned).
    fn on_pin_done(&mut self, seq: u64, outcome: PinOutcome) {
        if seq != self.pin_seq {
            return;
        }
        match outcome {
            PinOutcome::Resolved(rev) => {
                self.pin = PinState::Resolved;
                self.pin_resolved = Some(rev);
            }
            PinOutcome::NotFound | PinOutcome::Unavailable | PinOutcome::Failed => {
                self.pin = PinState::Failed;
            }
        }
    }

    fn resize(&mut self, size: (u16, u16)) {
        let dims = view_dims(size);
        if dims != self.dims {
            self.dims = dims;
            self.preview = viewport::new(dims.0, dims.1);
            self.preview.set_content(&self.preview_text);
        }
    }

    // ---- async glue (reads config(); not unit-tested) ----

    /// Start a verify for the current host/package: bump the token, show the spinner, and
    /// batch the nix check with a spinner tick. No hosts means nothing to preview.
    fn begin_verify(&mut self) -> Option<Cmd> {
        if self.hosts.is_empty() {
            self.verifying = false;
            return None;
        }
        let seq = self.mark_verifying();
        let pkg = self.pkg.value();
        let host = self.hosts[self.host_sel].clone();
        Some(command::batch(vec![verify_cmd(seq, pkg, host), spin_start(self.spinner.tick_msg())]))
    }

    /// Start a build for the current package, if `--build` was requested. Host-independent,
    /// so this is NOT called on a host switch.
    fn begin_build(&mut self) -> Option<Cmd> {
        config().build.as_ref()?; // None => --build not requested
        let seq = self.mark_building();
        let pkg = self.pkg.value();
        Some(command::batch(vec![build_cmd(seq, pkg), spin_start(self.build_spinner.tick_msg())]))
    }

    /// Start a pin resolve for the current package, if a version was requested. Host-
    /// independent, so this is NOT called on a host switch.
    fn begin_pin(&mut self) -> Option<Cmd> {
        config().pin.as_ref()?; // None => no version requested
        let version = self.version.clone()?;
        let seq = self.mark_resolving();
        let pkg = self.pkg.value();
        Some(command::batch(vec![pin_cmd(seq, pkg, version), spin_start(self.pin_spinner.tick_msg())]))
    }

    pub fn update(&mut self, msg: Msg, size: (u16, u16)) -> Step {
        if msg.downcast_ref::<WindowSizeMsg>().is_some() {
            self.resize(size);
            return Step::stay();
        }
        if let Some(done) = msg.downcast_ref::<VerifyDone>() {
            self.on_verify_done(done.seq, done.verified.clone());
            return Step::stay();
        }
        if let Some(done) = msg.downcast_ref::<BuildDone>() {
            self.on_build_done(done.seq, done.outcome);
            return Step::stay();
        }
        if let Some(done) = msg.downcast_ref::<PinDone>() {
            self.on_pin_done(done.seq, done.outcome.clone());
            return Step::stay();
        }
        if let Some(tick) = msg.downcast_ref::<spinner::TickMsg>() {
            // Route the tick to whichever spinner owns it (each has a distinct id), and
            // re-arm only while that spinner's work is still in flight.
            if tick.id == self.build_spinner.id() {
                let cmd = self.build_spinner.update(msg);
                return Step {
                    nav: Nav::Stay,
                    cmd: if self.build == BuildState::Building { cmd } else { None },
                };
            }
            if tick.id == self.pin_spinner.id() {
                let cmd = self.pin_spinner.update(msg);
                return Step {
                    nav: Nav::Stay,
                    cmd: if self.pin == PinState::Resolving { cmd } else { None },
                };
            }
            let cmd = self.spinner.update(msg);
            return Step { nav: Nav::Stay, cmd: if self.verifying { cmd } else { None } };
        }
        let Some((code, mods)) = key_of(&msg) else { return Step::stay() };

        // Ctrl-C and Esc always back out, regardless of focus (so they still work while the
        // package field has focus and would otherwise swallow the key).
        if matches!(code, KeyCode::Char('c')) && mods.contains(KeyModifiers::CONTROL) {
            return Step::nav(Nav::Back);
        }
        if code == KeyCode::Esc {
            return Step::nav(Nav::Back);
        }
        // Tab and the arrow keys move the focus selector between controls. Left/Right stay
        // control-specific (host switch, cursor in the package field).
        if matches!(code, KeyCode::Tab | KeyCode::Down) {
            self.focus_next();
            return Step::stay();
        }
        if matches!(code, KeyCode::BackTab | KeyCode::Up) {
            self.focus_prev();
            return Step::stay();
        }

        match self.focus {
            Focus::Host => match code {
                KeyCode::Left => {
                    if self.host_sel > 0 && self.set_host(self.host_sel - 1) {
                        Step { nav: Nav::Stay, cmd: self.begin_verify() }
                    } else {
                        Step::stay()
                    }
                }
                KeyCode::Right => {
                    if self.set_host(self.host_sel + 1) {
                        Step { nav: Nav::Stay, cmd: self.begin_verify() }
                    } else {
                        Step::stay()
                    }
                }
                _ => Step::stay(),
            },
            Focus::Package => match code {
                // Enter commits the edited package name, re-verifies, and re-runs the build
                // and pin resolve (both package-only, so they re-run here but not on a host
                // switch).
                KeyCode::Enter => {
                    let v = self.begin_verify();
                    let b = self.begin_build();
                    let p = self.begin_pin();
                    Step { nav: Nav::Stay, cmd: batch_all(vec![v, b, p]) }
                }
                _ => {
                    let cmd = self.pkg.update(msg);
                    Step { nav: Nav::Stay, cmd }
                }
            },
            Focus::Strict => match code {
                KeyCode::Char(' ') | KeyCode::Enter => {
                    self.toggle_strict();
                    Step::stay()
                }
                _ => Step::stay(),
            },
            Focus::Preview => {
                let cmd = self.preview.update(msg);
                Step { nav: Nav::Stay, cmd }
            }
            Focus::Apply => match code {
                KeyCode::Enter if self.apply_allowed() => Step::nav(Nav::Apply {
                    host: self.hosts[self.host_sel].clone(),
                    pkg: self.pkg.value(),
                    strict: self.strict,
                    version: self.version.clone(),
                    pin: self.pin_resolved.clone(),
                    no_abi_check: self.no_abi_check,
                }),
                _ => Step::stay(),
            },
            Focus::Cancel => match code {
                KeyCode::Enter => Step::nav(Nav::Back),
                _ => Step::stay(),
            },
        }
    }

    pub fn view(&self, size: (u16, u16)) -> String {
        if too_small(size) {
            return theme::dim().render("terminal too small \u{2013} resize to install");
        }
        if self.hosts.is_empty() {
            return format!(
                "{}\n{}",
                theme::chip(" install "),
                theme::dim().render("no hosts found under hosts/"),
            );
        }

        let host = &self.hosts[self.host_sel];
        let host_line = format!(
            "{}{}{}{}{}",
            marker(self.focus == Focus::Host),
            theme::dim().render("host   "),
            theme::amber().render("\u{2039} "),
            Style::new().bold(true).render(&host.name),
            theme::amber().render(" \u{203a}   \u{2190}/\u{2192}"),
        );
        let pkg_line = format!(
            "{}{}{}",
            marker(self.focus == Focus::Package),
            theme::dim().render("pkg    "),
            self.pkg.view(),
        );
        let strict_line = format!(
            "{}{}{} strict",
            marker(self.focus == Focus::Strict),
            theme::dim().render("check  "),
            theme::toggle(self.strict),
        );

        let verify_line =
            format!("{}{}{}", marker(false), theme::dim().render("verify "), self.verify_status());

        let build_line = (self.build != BuildState::Off).then(|| {
            let status = match self.build {
                BuildState::Building => format!("{} building", self.build_spinner.view()),
                BuildState::Ok => theme::good().render("\u{2713} builds"),
                BuildState::Failed => theme::bad().render("\u{2717} build failed"),
                BuildState::Skipped => theme::amber().render("\u{00b7} build skipped"),
                BuildState::Off => String::new(),
            };
            format!("{}{}{}", marker(false), theme::dim().render("build  "), status)
        });

        let pin_line = (self.pin != PinState::Off).then(|| {
            let status = match self.pin {
                PinState::Resolving => format!("{} resolving", self.pin_spinner.view()),
                PinState::Resolved => {
                    let rev = self.pin_resolved.as_deref().map(short_rev).unwrap_or_default();
                    theme::good().render(&format!("\u{2713} pinned {rev}"))
                }
                PinState::Failed => theme::bad().render("\u{2717} pin failed"),
                PinState::Off => String::new(),
            };
            format!("{}{}{}", marker(false), theme::dim().render("pin    "), status)
        });

        let preview_box = Style::new()
            .border(rounded_border())
            .border_foreground(theme::border(self.focus == Focus::Preview))
            .render(&self.preview.view());
        let preview_hdr = format!(
            "{}{}",
            marker(self.focus == Focus::Preview),
            theme::dim().render(&format!("{}.nix", host.name)),
        );

        let apply = self.button("apply", self.focus == Focus::Apply, self.apply_allowed());
        let cancel = self.button("cancel", self.focus == Focus::Cancel, true);
        let buttons = format!("{apply}  {cancel}");

        let hint = widgets::footer(&[
            ("tab", "move"),
            ("enter", "act"),
            ("\u{2190}/\u{2192}", "host"),
            ("space", "strict"),
            ("esc", "back"),
        ]);

        let mut lines = vec![theme::chip(" install "), host_line, pkg_line, strict_line, verify_line];
        if let Some(b) = build_line {
            lines.push(b);
        }
        if let Some(p) = pin_line {
            lines.push(p);
        }
        lines.extend([preview_hdr, preview_box, buttons, hint]);
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        join_vertical(LEFT, &refs)
    }

    fn verify_status(&self) -> String {
        let (r_txt, r) = match self.resolves {
            Resolve::Yes => ("\u{2713} resolves", theme::good()),
            Resolve::No => ("\u{2717} no such package", theme::bad()),
            Resolve::Skipped => ("\u{00b7} resolve skipped", theme::amber()),
        };
        if self.verifying {
            return format!("{} verifying", self.spinner.view());
        }
        let (p_txt, p) = match &self.parses {
            Parse::Ok => ("\u{2713} parses", theme::good()),
            Parse::Running => ("\u{2026} verifying", theme::amber()),
            Parse::Failed(_) => ("\u{2717} parse failed", theme::bad()),
            Parse::Skipped => ("\u{00b7} parse skipped", theme::amber()),
        };
        format!("{}  {}", r.render(r_txt), p.render(p_txt))
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

/// Extract the key code and modifiers if the message is a key press. `KeyCode`/`KeyModifiers`
/// are `Copy`, so we read them out and leave `msg` owned for forwarding to a widget.
fn key_of(msg: &Msg) -> Option<(KeyCode, KeyModifiers)> {
    msg.downcast_ref::<KeyMsg>().map(|k| (k.key, k.modifiers))
}

/// The nix verify, off the event-loop thread so the spinner keeps ticking. Resolves to a
/// `VerifyDone` carrying the token so a stale result (superseded verify) is discarded.
fn verify_cmd(seq: u64, pkg: String, host: HostInfo) -> Cmd {
    let verify = config().verify.clone();
    Box::pin(async move {
        match tokio::task::spawn_blocking(move || verify(&pkg, &host)).await {
            Ok(verified) => Some(Box::new(VerifyDone { seq, verified }) as Msg),
            Err(_) => None,
        }
    })
}

/// The package build, off the event-loop thread. Resolves to a `BuildDone` with the token so
/// a stale build (package edited again) is discarded. Only called when `config().build` is
/// `Some`.
fn build_cmd(seq: u64, pkg: String) -> Cmd {
    let build = config().build.clone().expect("build fn present when begin_build ran");
    Box::pin(async move {
        match tokio::task::spawn_blocking(move || build(&pkg)).await {
            Ok(outcome) => Some(Box::new(BuildDone { seq, outcome }) as Msg),
            Err(_) => None,
        }
    })
}

/// The pin resolve, off the event-loop thread. Resolves to a `PinDone` with the token so a
/// stale resolve (package edited again) is discarded. Only called when `config().pin` is
/// `Some` and a version was requested.
fn pin_cmd(seq: u64, pkg: String, version: String) -> Cmd {
    let pin = config().pin.clone().expect("pin fn present when begin_pin ran");
    Box::pin(async move {
        match tokio::task::spawn_blocking(move || pin(&pkg, &version)).await {
            Ok(outcome) => Some(Box::new(PinDone { seq, outcome }) as Msg),
            Err(_) => None,
        }
    })
}

/// Kick the spinner by emitting its first tick; the spinner's own `update` re-arms after that.
fn spin_start(tick: spinner::TickMsg) -> Cmd {
    Box::pin(async move { Some(Box::new(tick) as Msg) })
}

/// Batch together whichever of these commands are present, or `None` if all are absent.
/// Generalises the verify/build/pin fan-out at `enter` and on a package edit, each of which
/// may or may not have a command to run.
fn batch_all(cmds: Vec<Option<Cmd>>) -> Option<Cmd> {
    let mut present: Vec<Cmd> = cmds.into_iter().flatten().collect();
    match present.len() {
        0 => None,
        1 => present.pop(),
        _ => Some(command::batch(present)),
    }
}

/// A short form of a nixpkgs commit for the pin status line (first 7 chars, git-style).
fn short_rev(rev: &str) -> String {
    rev.chars().take(7).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn host(name: &str) -> HostInfo {
        HostInfo { name: name.into(), default: false, path: PathBuf::from(format!("hosts/{name}.kdl")) }
    }

    /// A model built without the program's `config()` global, for testing the pure logic.
    fn model(hosts: usize) -> InstallModel {
        let mut pkg = textinput::new();
        pkg.set_value("ripgrep");
        InstallModel {
            pkg,
            hosts: (0..hosts).map(|i| host(&format!("h{i}"))).collect(),
            host_sel: 0,
            strict: false,
            no_abi_check: false,
            focus: Focus::Package,
            resolves: Resolve::Yes,
            parses: Parse::Ok,
            preview: viewport::new(40, 5),
            preview_text: String::new(),
            verifying: false,
            seq: 0,
            spinner: new_spinner(),
            build: BuildState::Off,
            build_spinner: new_spinner(),
            build_seq: 0,
            version: None,
            pin: PinState::Off,
            pin_spinner: new_spinner(),
            pin_seq: 0,
            pin_resolved: None,
            dims: (40, 5),
        }
    }

    fn verified(resolves: Resolve, parses: Parse) -> Verified {
        Verified { preview: "environment.systemPackages = [ pkgs.ripgrep ];".into(), resolves, parses }
    }

    #[test]
    fn focus_cycles_forward_and_back_wrapping() {
        let mut m = model(2);
        m.focus = Focus::Host;
        m.focus_next();
        assert_eq!(m.focus, Focus::Package);
        m.focus_prev();
        assert_eq!(m.focus, Focus::Host);
        m.focus_prev();
        assert_eq!(m.focus, Focus::Cancel, "wraps to the last control");
    }

    #[test]
    fn arrow_keys_move_the_focus_selector() {
        let mut m = model(2);
        assert_eq!(m.focus, Focus::Package);
        let key = |c| Box::new(KeyMsg { key: c, modifiers: KeyModifiers::NONE }) as Msg;
        m.update(key(KeyCode::Down), (80, 24));
        assert_eq!(m.focus, Focus::Strict, "down moves to the next control");
        m.update(key(KeyCode::Up), (80, 24));
        assert_eq!(m.focus, Focus::Package, "up moves back");
    }

    #[test]
    fn set_host_moves_and_clamps() {
        let mut m = model(3);
        assert!(m.set_host(2));
        assert_eq!(m.host_sel, 2);
        assert!(!m.set_host(2), "no change when already selected");
        assert!(!m.set_host(9), "out of range is refused");
        assert_eq!(m.host_sel, 2);
    }

    #[test]
    fn toggle_strict_flips() {
        let mut m = model(1);
        assert!(!m.strict);
        m.toggle_strict();
        assert!(m.strict);
    }

    #[test]
    fn apply_blocked_while_verifying_and_without_hosts() {
        let mut m = model(1);
        assert!(m.apply_allowed());
        m.verifying = true;
        assert!(!m.apply_allowed(), "in-flight verify blocks apply");
        let mut none = model(0);
        assert!(!none.apply_allowed(), "no hosts blocks apply");
        none.verifying = false;
    }

    #[test]
    fn apply_gating_matches_resolve_and_parse_under_strict() {
        let mut m = model(1);
        m.resolves = Resolve::No;
        assert!(!m.apply_allowed(), "unresolved never applies");

        m.resolves = Resolve::Skipped;
        m.parses = Parse::Skipped;
        m.strict = false;
        assert!(m.apply_allowed(), "skips are fine without --strict");
        m.strict = true;
        assert!(!m.apply_allowed(), "--strict rejects skipped checks");

        m.strict = false;
        m.parses = Parse::Failed("boom".into());
        assert!(!m.apply_allowed(), "a parse failure never applies");
    }

    #[test]
    fn verify_done_updates_status_and_preview() {
        let mut m = model(1);
        let seq = m.mark_verifying();
        assert!(m.verifying);
        m.on_verify_done(seq, verified(Resolve::No, Parse::Failed("nope".into())));
        assert!(!m.verifying);
        assert_eq!(m.resolves, Resolve::No);
        assert_eq!(m.parses, Parse::Failed("nope".into()));
        assert!(m.preview_text.contains("systemPackages"));
    }

    #[test]
    fn stale_verify_result_is_ignored() {
        let mut m = model(1);
        let first = m.mark_verifying();
        let _second = m.mark_verifying(); // supersedes the first
        m.on_verify_done(first, verified(Resolve::No, Parse::Ok));
        assert!(m.verifying, "the superseded result must not clear the in-flight state");
        assert_eq!(m.resolves, Resolve::Yes, "stale resolve discarded");
    }

    #[test]
    fn resize_rebuilds_the_viewport_and_keeps_content() {
        let mut m = model(1);
        m.set_preview("a\nb\nc".into());
        m.resize((120, 40));
        assert_eq!(m.dims, view_dims((120, 40)));
        assert!(m.preview.view().contains('a'), "content survives a resize");
    }

    #[test]
    fn view_shows_controls_at_a_normal_size() {
        let m = model(2);
        let v = m.view((80, 24));
        assert!(v.contains("install"), "title");
        assert!(v.contains("ripgrep"), "package field");
        assert!(v.contains("h0"), "host");
        assert!(v.contains("apply"), "apply button");
        assert!(v.contains("cancel"), "cancel button");
    }

    #[test]
    fn view_shows_a_resize_hint_when_tiny() {
        let m = model(1);
        assert!(m.view((10, 5)).contains("resize"));
    }

    #[test]
    fn view_reports_no_hosts() {
        let m = model(0);
        assert!(m.view((80, 24)).contains("no hosts"));
    }

    #[test]
    fn build_gating_blocks_apply_until_it_succeeds() {
        let mut m = model(1);
        m.build = BuildState::Building; // requested + in flight
        assert!(!m.apply_allowed(), "in-flight build blocks apply");
        m.build = BuildState::Failed;
        assert!(!m.apply_allowed(), "failed build blocks apply");
        m.build = BuildState::Ok;
        assert!(m.apply_allowed(), "successful build allows apply");
    }

    #[test]
    fn build_skipped_gates_only_under_strict() {
        let mut m = model(1);
        m.build = BuildState::Skipped;
        m.strict = false;
        assert!(m.apply_allowed(), "skipped build is fine without --strict");
        m.strict = true;
        assert!(!m.apply_allowed(), "--strict rejects a skipped build");
    }

    #[test]
    fn build_off_does_not_affect_gating() {
        let mut m = model(1); // resolves Yes, parses Ok by default
        m.build = BuildState::Off;
        assert!(m.apply_allowed(), "no --build means the build never gates");
    }

    #[test]
    fn on_build_done_sets_state_and_ignores_stale() {
        let mut m = model(1);
        m.build = BuildState::Building;
        let seq = m.mark_building(); // bumps build_seq, sets Building
        m.on_build_done(seq, BuildOutcome::Ok);
        assert_eq!(m.build, BuildState::Ok);
        let stale = seq; // superseded
        m.mark_building();
        m.on_build_done(stale, BuildOutcome::Failed);
        assert_eq!(m.build, BuildState::Building, "stale build result discarded");
    }

    #[test]
    fn pin_gating_blocks_apply_until_resolved() {
        let mut m = model(1);
        m.pin = PinState::Resolving;
        assert!(!m.apply_allowed(), "in-flight resolve blocks apply");
        m.pin = PinState::Failed;
        assert!(!m.apply_allowed(), "failed resolve blocks apply");
        m.pin = PinState::Resolved;
        assert!(m.apply_allowed(), "resolved allows apply");
    }

    #[test]
    fn pin_off_does_not_affect_gating() {
        let mut m = model(1);
        m.pin = PinState::Off;
        assert!(m.apply_allowed());
    }

    #[test]
    fn on_pin_done_ignores_stale() {
        let mut m = model(1);
        let seq = m.mark_resolving();
        m.mark_resolving();
        m.on_pin_done(seq, PinOutcome::Resolved("abc123".into()));
        assert_eq!(m.pin, PinState::Resolving, "stale resolve discarded");
    }
}
