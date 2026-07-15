//! The interactive knixl TUI (bubbletea-rs + lipgloss). `knixl tui` opens the hub.
//!
//! bubbletea's `Model::init()` takes no arguments, so project context is stashed in a
//! `OnceLock` before the program runs. Each screen exposes a pure `update(&KeyMsg) -> Nav`
//! reducer and a `view(size) -> String`, tested directly; the bubbletea `Model` impl is a
//! thin layer that downcasts messages and maps `Nav` to a command.

mod home;
mod theme;

use std::path::PathBuf;
use std::sync::OnceLock;

use bubbletea_rs::event::{KeyMsg, QuitMsg, WindowSizeMsg};
use bubbletea_rs::{command, Cmd, Model, Msg, Program};

use home::HomeModel;

static CONFIG: OnceLock<TuiConfig> = OnceLock::new();

/// Project context the screens need, injected before the program runs.
pub struct TuiConfig {
    // Read by the Install/Browse/Author screens (later checkpoints).
    #[allow(dead_code)]
    pub root: PathBuf,
}

fn config() -> &'static TuiConfig {
    CONFIG.get().expect("TUI config set before run")
}

/// A screen's navigation intent, returned by its pure reducer.
pub enum Nav {
    Stay,
    Quit,
    // The target screen key is routed by `App::apply` once those screens exist.
    #[allow(dead_code)]
    Goto(&'static str),
}

enum Screen {
    Home(HomeModel),
}

pub struct App {
    size: (u16, u16),
    screen: Screen,
}

impl Model for App {
    fn init() -> (Self, Option<Cmd>) {
        let _ = config(); // ensure it was set
        (App { size: (80, 24), screen: Screen::Home(HomeModel::new()) }, None)
    }

    fn update(&mut self, msg: Msg) -> Option<Cmd> {
        if msg.downcast_ref::<QuitMsg>().is_some() {
            return None;
        }
        if let Some(ws) = msg.downcast_ref::<WindowSizeMsg>() {
            self.size = (ws.width, ws.height);
            return None;
        }
        if let Some(key) = msg.downcast_ref::<KeyMsg>() {
            let nav = match &mut self.screen {
                Screen::Home(h) => h.update(key),
            };
            return self.apply(nav);
        }
        None
    }

    fn view(&self) -> String {
        match &self.screen {
            Screen::Home(h) => h.view(self.size),
        }
    }
}

impl App {
    fn apply(&mut self, nav: Nav) -> Option<Cmd> {
        match nav {
            Nav::Quit => Some(command::quit()),
            // Install / Browse / Author screens land in later checkpoints.
            Nav::Goto(_) => None,
            Nav::Stay => None,
        }
    }
}

/// Run the TUI. Sets the config, builds a tokio runtime, and drives the bubbletea program.
pub fn run(root: PathBuf) -> Result<(), String> {
    let _ = CONFIG.set(TuiConfig { root });
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async {
        let program = Program::<App>::builder().build().map_err(|e| e.to_string())?;
        program.run().await.map_err(|e| e.to_string())?;
        Ok::<(), String>(())
    })
}

#[cfg(test)]
mod tests {
    use super::home::HomeModel;
    use super::Nav;
    use bubbletea_rs::event::KeyMsg;
    use crossterm::event::{KeyCode, KeyModifiers};

    fn key(code: KeyCode) -> KeyMsg {
        KeyMsg { key: code, modifiers: KeyModifiers::NONE }
    }

    #[test]
    fn home_down_up_moves_selection_clamped() {
        let mut h = HomeModel::new();
        assert_eq!(h.sel, 0);
        assert!(matches!(h.update(&key(KeyCode::Down)), Nav::Stay));
        assert_eq!(h.sel, 1);
        assert!(matches!(h.update(&key(KeyCode::Up)), Nav::Stay));
        assert_eq!(h.sel, 0);
        h.update(&key(KeyCode::Up)); // clamp at top
        assert_eq!(h.sel, 0);
    }

    #[test]
    fn home_enter_routes_or_quits() {
        let mut h = HomeModel::new();
        // first item -> install
        assert!(matches!(h.update(&key(KeyCode::Enter)), Nav::Goto("install")));
        // move to the last (Quit) and enter
        for _ in 0..10 {
            h.update(&key(KeyCode::Down));
        }
        assert!(matches!(h.update(&key(KeyCode::Enter)), Nav::Quit));
    }

    #[test]
    fn home_q_and_esc_quit() {
        let mut h = HomeModel::new();
        assert!(matches!(h.update(&key(KeyCode::Char('q'))), Nav::Quit));
        assert!(matches!(h.update(&key(KeyCode::Esc)), Nav::Quit));
    }

    #[test]
    fn home_view_lists_the_menu() {
        let v = HomeModel::new().view((80, 24));
        for item in ["Install a package", "Browse modules", "New module", "Quit"] {
            assert!(v.contains(item), "menu shows {item}: {v}");
        }
    }
}
