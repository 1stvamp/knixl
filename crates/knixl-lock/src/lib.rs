//! Lockfile model and the reconcile state machine. SPEC-GRADE SKETCH.
pub mod model;
pub mod reconcile;

pub use model::Lock;
pub use reconcile::{Apply, FilePlan, FileState, Plan, VersionSkew};
