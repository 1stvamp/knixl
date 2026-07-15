//! The interactive knixl TUI (bubbletea-rs + lipgloss). `knixl tui` opens the hub; `knixl
//! install <pkg>` opens the Install screen directly.
//!
//! bubbletea's `Model::init()` takes no arguments, so everything the screens need (the host
//! list, the entry point, and an injected verify function) is stashed in a `OnceLock` before
//! the program runs. Each screen exposes a `update(msg, size) -> Step` reducer and a
//! `view(size) -> String`; the bubbletea `Model` impl is a thin layer that forwards messages
//! and turns a `Step` into a command. The pure decision logic inside each screen is unit
//! tested; only the runtime glue (spawning the program, real key reads, async `Cmd`s) is not.

mod author;
mod browse;
mod home;
mod install;
mod theme;
mod widgets;

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use bubbletea_rs::event::{QuitMsg, WindowSizeMsg};
use bubbletea_rs::{command, Cmd, Model, Msg, Program};
use lipgloss::{place, CENTER};

use author::AuthorModel;
use browse::BrowseModel;
use home::HomeModel;
use install::InstallModel;

pub use install::{Parse, Resolve};

use knixl_pipeline::install::HostInfo;

static CONFIG: OnceLock<TuiConfig> = OnceLock::new();

/// The result of the async nix verify, handed to the Install screen.
#[derive(Debug, Clone)]
pub struct Verified {
    pub preview: String,
    pub resolves: Resolve,
    pub parses: Parse,
}

/// Runs the (blocking) nix verify for a package on a host. Injected so tests and the real CLI
/// supply their own; it is `Send + Sync` so the Install screen can run it off the event loop.
pub type VerifyFn = Arc<dyn Fn(&str, &HostInfo) -> Verified + Send + Sync>;

/// A registered module as the Browse screen sees it: its claimed node, a kind tag, the
/// rendered schema doc, and a skeleton to splice into a host. Precomputed by the CLI so the
/// TUI never touches the (non-`Send`) registry.
#[derive(Debug, Clone)]
pub struct BrowseModule {
    pub node: String,
    pub kind: String,
    pub doc: String,
    pub skeleton: String,
}

/// How the TUI was launched.
pub enum Entry {
    /// `knixl tui`: start at Home.
    Hub,
    /// `knixl install <pkg>`: open the Install screen with the package prefilled.
    Install { pkg: String, strict: bool, host: Option<String> },
}

/// What the session decided, returned by `run` for the CLI to act on.
pub enum Outcome {
    /// Nothing to do (plain quit, or backed out of Home).
    Quit,
    /// The install screen was cancelled.
    Cancelled,
    /// Apply this package to this host.
    Install { host: HostInfo, pkg: String, strict: bool },
    /// Scaffold this module's node into this host's KDL.
    Insert { host: HostInfo, node: String, skeleton: String },
    /// Write a new declarative module manifest (`modules/<name>/knixl-module.kdl`).
    Scaffold { name: String, manifest: String },
}

/// Everything the screens read, injected before the program runs.
pub struct TuiConfig {
    // Read by the Browse/Author screens (later checkpoints).
    #[allow(dead_code)]
    pub root: PathBuf,
    pub hosts: Vec<HostInfo>,
    pub entry: Entry,
    pub verify: VerifyFn,
    pub modules: Vec<BrowseModule>,
}

fn config() -> &'static TuiConfig {
    CONFIG.get().expect("TUI config set before run")
}

/// A screen's navigation intent, returned by its reducer.
pub enum Nav {
    Stay,
    Quit,
    /// Back out of the current screen (to Home, or end the session if launched directly).
    Back,
    /// Open another screen by key.
    Goto(&'static str),
    /// Commit the install and end the session.
    Apply { host: HostInfo, pkg: String, strict: bool },
    /// Scaffold a module node into a host and end the session.
    Insert { host: HostInfo, node: String, skeleton: String },
    /// Write a new module manifest and end the session.
    Scaffold { name: String, manifest: String },
}

/// A reducer's result: a navigation intent plus an optional command to run.
pub struct Step {
    pub nav: Nav,
    pub cmd: Option<Cmd>,
}

impl Step {
    fn stay() -> Step {
        Step { nav: Nav::Stay, cmd: None }
    }
    fn nav(nav: Nav) -> Step {
        Step { nav, cmd: None }
    }
}

enum Screen {
    Home(Box<HomeModel>),
    // Boxed: these own widget state and are large, so keep the enum small.
    Install(Box<InstallModel>),
    Browse(Box<BrowseModel>),
    Author(Box<AuthorModel>),
}

pub struct App {
    size: (u16, u16),
    screen: Screen,
    outcome: Outcome,
}

impl Model for App {
    fn init() -> (Self, Option<Cmd>) {
        let size = (80, 24);
        let (screen, cmd) = match config().entry {
            Entry::Install { .. } => {
                let (m, cmd) = InstallModel::enter(size);
                (Screen::Install(Box::new(m)), cmd)
            }
            Entry::Hub => (Screen::Home(Box::new(HomeModel::new())), None),
        };
        // Ask for the real terminal size up front so the first frame fills the screen (rather
        // than the 80x24 default), batched with any screen's own init command.
        let init_cmd = match cmd {
            Some(c) => command::batch(vec![command::window_size(), c]),
            None => command::window_size(),
        };
        (App { size, screen, outcome: Outcome::Quit }, Some(init_cmd))
    }

    fn update(&mut self, msg: Msg) -> Option<Cmd> {
        if msg.downcast_ref::<QuitMsg>().is_some() {
            return None;
        }
        if let Some(ws) = msg.downcast_ref::<WindowSizeMsg>() {
            self.size = (ws.width, ws.height);
        }
        let step = match &mut self.screen {
            Screen::Home(h) => h.update(msg, self.size),
            Screen::Install(i) => i.update(msg, self.size),
            Screen::Browse(b) => b.update(msg, self.size),
            Screen::Author(a) => a.update(msg, self.size),
        };
        self.apply(step)
    }

    fn view(&self) -> String {
        let content = match &self.screen {
            Screen::Home(h) => h.view(self.size),
            Screen::Install(i) => i.view(self.size),
            Screen::Browse(b) => b.view(self.size),
            Screen::Author(a) => a.view(self.size),
        };
        // Centre the screen in the full terminal so the alternate screen is used end to end.
        let (w, h) = self.size;
        place(w as i32, h as i32, CENTER, CENTER, &content, &[])
    }
}

impl App {
    fn apply(&mut self, step: Step) -> Option<Cmd> {
        match step.nav {
            Nav::Stay => step.cmd,
            Nav::Quit => Some(command::quit()),
            Nav::Apply { host, pkg, strict } => {
                self.outcome = Outcome::Install { host, pkg, strict };
                Some(command::quit())
            }
            Nav::Insert { host, node, skeleton } => {
                self.outcome = Outcome::Insert { host, node, skeleton };
                Some(command::quit())
            }
            Nav::Scaffold { name, manifest } => {
                self.outcome = Outcome::Scaffold { name, manifest };
                Some(command::quit())
            }
            Nav::Back => match config().entry {
                // Launched straight into a screen: backing out ends the session.
                Entry::Install { .. } => {
                    self.outcome = Outcome::Cancelled;
                    Some(command::quit())
                }
                Entry::Hub => {
                    self.screen = Screen::Home(Box::new(HomeModel::new()));
                    None
                }
            },
            Nav::Goto("install") => {
                let (m, cmd) = InstallModel::enter(self.size);
                self.screen = Screen::Install(Box::new(m));
                cmd
            }
            Nav::Goto("browse") => {
                self.screen = Screen::Browse(Box::new(BrowseModel::enter(self.size)));
                None
            }
            Nav::Goto("author") => {
                self.screen = Screen::Author(Box::new(AuthorModel::enter(self.size)));
                None
            }
            Nav::Goto(_) => None,
        }
    }
}

/// Run the TUI, returning what the session decided. Sets the config, builds a tokio runtime,
/// drives the bubbletea program, and reads the outcome off the final model.
pub fn run(
    entry: Entry,
    root: PathBuf,
    hosts: Vec<HostInfo>,
    verify: VerifyFn,
    modules: Vec<BrowseModule>,
) -> Result<Outcome, String> {
    let _ = CONFIG.set(TuiConfig { root, hosts, entry, verify, modules });
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async {
        let program =
            Program::<App>::builder().alt_screen(true).build().map_err(|e| e.to_string())?;
        let app = program.run().await.map_err(|e| e.to_string())?;
        Ok(app.outcome)
    })
}
