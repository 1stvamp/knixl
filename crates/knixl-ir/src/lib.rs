//! knixl IR: a constrained subset of Nix (module bodies only) plus a deterministic emitter.
//!
//! SPEC-GRADE SKETCH. Does not compile as delivered: helper bodies are elided.
//! See HANDOFF.md. The types and the Emit trait shape are the intent; preserve them.
//!
//! Determinism is load-bearing (the lock depends on it): AttrSet is a BTreeMap so key
//! order is fixed by construction, lists preserve source order, no HashMap in emit paths.

pub mod expr;
pub mod module;
pub mod emit;
pub mod hoist;

pub use expr::{AttrKey, AttrPath, Binding, Formals, NixExpr, Priority, RawNix};
pub use module::{Assignment, ModuleRef, NixModule, Provenance};
pub use emit::{Emit, Writer};
