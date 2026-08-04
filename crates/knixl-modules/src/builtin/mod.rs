//! Built-in Rust modules: used when the logic cannot be straight-line substitution.
pub mod backups;
pub mod disko;
pub mod guest;
pub mod host;
pub mod incus;
pub mod nix_ld;
pub mod os;
pub mod package;
pub mod postgres;
pub mod raw_nix;

use crate::Registry;

/// Register every built-in. Called at startup before scanning modules/ for declarative ones.
pub fn register_builtins(reg: &mut Registry) {
    let _ = reg.register(Box::new(host::Host::new()));
    let _ = reg.register(Box::new(postgres::Postgres::new()));
    let _ = reg.register(Box::new(backups::Backups::new()));
    let _ = reg.register(Box::new(raw_nix::RawNixModule::new()));
    let _ = reg.register(Box::new(package::PackageModule::new()));
    let _ = reg.register(Box::new(disko::Disko::new()));
    let _ = reg.register(Box::new(incus::Incus::new()));
    let _ = reg.register(Box::new(os::Os::new()));
    let _ = reg.register(Box::new(guest::Guest::new()));
    let _ = reg.register(Box::new(nix_ld::NixLd::new()));
    // ... more as they land.
}
