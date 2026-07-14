use crate::expr::{AttrPath, Binding, Formals, NixExpr, Priority};

/// A module file is not a NixExpr: it has a fixed shape, always emitted the same way.
#[derive(Debug, Clone)]
pub struct NixModule {
    pub header: Formals,           // { config, lib, pkgs, ... }:
    pub imports: Vec<NixExpr>,     // imports = [ ... ];  (kept separate so it emits first)
    pub lets: Vec<Binding>,        // optional hoisted let-block (generator pass)
    pub body: Vec<Assignment>,
    pub provenance: Provenance,    // drives the header comment + lock entry
}

/// One option assignment: services.nginx.virtualHosts."example.com" = <value>;
#[derive(Debug, Clone)]
pub struct Assignment {
    pub path: AttrPath,
    pub value: NixExpr,
    pub priority: Option<Priority>,   // lib.mkForce / mkDefault / mkOverride n
    pub condition: Option<NixExpr>,   // wrapped in lib.mkIf (Rust-only for now)
    pub doc: Option<String>,          // emitted as a comment above the line
}

#[derive(Debug, Clone)]
pub struct Provenance {
    pub tool_version: semver::Version,
    pub modules: Vec<ModuleRef>,           // which modules produced this file
    pub sources: Vec<std::path::PathBuf>,  // KDL inputs that fed it
}

#[derive(Debug, Clone)]
pub struct ModuleRef { pub name: String, pub version: semver::Version }
