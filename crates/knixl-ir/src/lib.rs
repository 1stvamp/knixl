//! knixl IR: a constrained subset of Nix (module bodies only) plus a deterministic emitter.
//!
//! Determinism is load-bearing (the lock depends on it): AttrSet is a BTreeMap so key
//! order is fixed by construction, lists preserve source order, no HashMap in emit paths.

pub mod emit;
pub mod expr;
pub mod hoist;
pub mod module;

pub use emit::{Emit, Writer};
pub use expr::{AttrKey, AttrPath, Binding, Formals, NixExpr, Priority, RawNix};
pub use module::{Assignment, ModuleRef, NixModule, Provenance};
