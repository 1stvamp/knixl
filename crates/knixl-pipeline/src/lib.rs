//! Orchestration: KDL source text in, generated + formatted Nix files out.
//!
//! This is the single generation entry point. The CLI's `Ctx::load` and the golden
//! tests both call `generate`, so the pipeline is exercised the same way everywhere.
//! It returns bytes and writes nothing; the caller (or `Plan::compute`) decides what
//! reaches disk.
//!
//! SPEC-GRADE SKETCH: the orchestration glue here is real, but the stages it drives
//! (schema validation, `lower`, the emit helpers, the formatter) are still elided in
//! their own crates, so calling `generate` panics until Phase 1 fills them in. That is
//! deliberate: the golden harness is red on purpose and turns green as the stubs land.

use std::collections::BTreeMap;
use std::path::PathBuf;

use kdl::{KdlDocument, KdlNode};
use knixl_ir::{Assignment, Emit, Formals, ModuleRef, NixExpr, NixModule, Provenance, Writer};
use knixl_kdl::{parse, ParseError};
use knixl_modules::{Bucket, LowerCtx, LowerError, Registry, Scope};
use knixl_nix::{FormatError, Formatter};
use semver::Version;

/// One KDL input: its repo-relative path (for provenance) and its contents.
pub struct HostSource {
    pub path: PathBuf,
    pub src: String,
}

/// One generated output file, post-format. Not written yet; that is the caller's job.
pub struct GeneratedFile {
    pub path: PathBuf,        // e.g. generated/hosts/web.nix
    pub text: String,         // formatted Nix, the thing that gets hashed
    pub from: PathBuf,        // the KDL input it derived from
    pub modules: Vec<String>, // modules that contributed (drives the lock entry)
}

#[derive(Debug, thiserror::Error)]
pub enum GenerateError {
    #[error(transparent)]
    Parse(#[from] ParseError),
    #[error("no module claims node '{0}'")]
    UnknownNode(String),
    #[error("input validation failed:\n{}", .0.join("\n"))]
    Validation(Vec<String>),
    #[error(transparent)]
    Lower(#[from] LowerError),
    #[error(transparent)]
    Format(#[from] FormatError),
}

/// Generate every output file for the given hosts, deterministically.
pub fn generate(
    hosts: &[HostSource],
    registry: &Registry,
    formatter: &Formatter,
    tool: &Version,
) -> Result<Vec<GeneratedFile>, GenerateError> {
    let mut out = Vec::new();
    for host in hosts {
        out.extend(generate_one(host, registry, formatter, tool)?);
    }
    Ok(out)
}

fn generate_one(
    host: &HostSource,
    registry: &Registry,
    formatter: &Formatter,
    tool: &Version,
) -> Result<Vec<GeneratedFile>, GenerateError> {
    let doc: KdlDocument = parse(&host.src)?;
    let host_name = first_arg_str(doc.nodes().first().ok_or_else(|| {
        GenerateError::Validation(vec![format!("{}: no top-level node", host.path.display())])
    })?)
    .unwrap_or_else(|| "host".to_string());

    // Dispatch every top-level node to its module and collect the lowered assignments,
    // keyed by output file. Container modules (host) fold their children in via lower(),
    // so the top level is usually a single `host` node.
    let mut files: BTreeMap<String, Vec<Assignment>> = BTreeMap::new();
    let mut modules: Vec<ModuleRef> = Vec::new();
    let mut diags: Vec<knixl_modules::Diagnostic> = Vec::new();

    for node in doc.nodes() {
        let name = node.name().value();
        let module = registry
            .get(name)
            .ok_or_else(|| GenerateError::UnknownNode(name.to_string()))?;

        module.schema().validate(node).map_err(|ds| {
            GenerateError::Validation(ds.into_iter().map(|d| d.message).collect())
        })?;

        let id = module.id();
        modules.push(ModuleRef { name: id.name, version: id.version });

        let mut ctx = LowerCtx::new(
            Scope { host: host_name.clone() },
            registry,
            &mut diags,
        );
        let output = module.lower(node, &mut ctx)?;

        for unit in output.units {
            let key = bucket_key(&unit.bucket, &host_name);
            files.entry(key).or_default().push(unit.assignment);
        }
    }

    // Named side-files (anything not the host's own file). The host file imports them.
    let side_files: Vec<String> =
        files.keys().filter(|k| *k != &host_name).cloned().collect();

    let module_names: Vec<String> = modules.iter().map(|m| m.name.clone()).collect();

    let mut generated = Vec::new();
    for (key, body) in files {
        let imports = if key == host_name {
            side_files
                .iter()
                .map(|n| NixExpr::Path(PathBuf::from(format!("./{n}.nix"))))
                .collect()
        } else {
            Vec::new()
        };

        let module = NixModule {
            header: module_header(),
            imports,
            lets: Vec::new(), // let-hoisting is a later pass (Phase 5)
            body,
            provenance: Provenance {
                tool_version: tool.clone(),
                // TODO(phase-2): per-file module attribution needs lower() to report which
                // module produced each unit; for now we record every module on every file.
                modules: modules.clone(),
                sources: vec![host.path.clone()],
            },
        };

        let mut w = Writer::new();
        module.emit(&mut w);
        let text = formatter.format(&w.into_string())?;

        generated.push(GeneratedFile {
            path: PathBuf::from(format!("generated/hosts/{key}.nix")),
            text,
            from: host.path.clone(),
            modules: module_names.clone(),
        });
    }
    Ok(generated)
}

fn bucket_key(bucket: &Bucket, host_name: &str) -> String {
    match bucket {
        Bucket::Default => host_name.to_string(),
        Bucket::Named(name) => name.clone(),
    }
}

/// Every generated module has the same head: `{ config, lib, pkgs, ... }:`.
fn module_header() -> Formals {
    Formals {
        args: vec!["config".into(), "lib".into(), "pkgs".into()],
        ellipsis: true,
    }
}

/// First positional (unnamed) argument of a node, as a string.
fn first_arg_str(node: &KdlNode) -> Option<String> {
    node.entries()
        .iter()
        .find(|e| e.name().is_none())
        .and_then(|e| e.value().as_string())
        .map(str::to_string)
}
