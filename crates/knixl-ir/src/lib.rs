//! knixl IR: a constrained subset of Nix (module bodies only) plus a deterministic emitter.
//!
//! Determinism invariants are load-bearing (see docs/01-architecture.md).

pub mod emit;
pub mod expr;
pub mod hoist;
pub mod module;

pub use emit::{Emit, Writer};
pub use expr::{AttrKey, AttrPath, Binding, Formals, NixExpr, Priority, RawNix};
pub use module::{Assignment, ModuleRef, NixModule, Provenance};
