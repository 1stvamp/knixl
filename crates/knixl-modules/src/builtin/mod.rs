//! Built-in Rust modules: used when the logic cannot be straight-line substitution.
pub mod host;
pub mod postgres;

use crate::Registry;

/// Register every built-in. Called at startup before scanning modules/ for declarative ones.
pub fn register_builtins(reg: &mut Registry) {
    let _ = reg.register(Box::new(host::Host::new()));
    let _ = reg.register(Box::new(postgres::Postgres::new()));
    // ... more as they land.
}
