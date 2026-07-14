//! Orchestration: KDL source text in, generated + formatted Nix files out.
//!
//! This is the single generation entry point. The CLI's `Ctx::load` and the golden
//! tests both call `generate`, so the pipeline is exercised the same way everywhere.
//! It returns bytes and writes nothing; the caller (or `Plan::compute`) decides what
//! reaches disk.
//!
//! The pipeline runs end to end (parse, dispatch, lower, emit, format). The byte-for-byte
//! golden tests additionally need `nixfmt` on PATH and regenerated `examples/expected/`,
//! so they stay `#[ignore]`d; the interpreter and reconcile logic are covered by unit tests.

pub mod gather;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use kdl::{KdlDocument, KdlNode};
use knixl_ir::{Assignment, Emit, Formals, ModuleRef, NixExpr, NixModule, Provenance, RawNix, Writer};
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
    let mut raw_files: BTreeMap<String, Vec<RawNix>> = BTreeMap::new();
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
        for r in output.raw {
            let key = bucket_key(&r.bucket, &host_name);
            raw_files.entry(key).or_default().push(r.raw);
        }
    }

    // Every output file: any bucket that produced assignments or raw passthrough.
    let mut keys: BTreeSet<String> = files.keys().cloned().collect();
    keys.extend(raw_files.keys().cloned());

    // Named side-files (anything not the host's own file). The host file imports them.
    let side_files: Vec<String> = keys.iter().filter(|k| *k != &host_name).cloned().collect();

    let module_names: Vec<String> = modules.iter().map(|m| m.name.clone()).collect();

    let mut generated = Vec::new();
    for key in keys {
        let body = files.remove(&key).unwrap_or_default();
        let raw = raw_files.remove(&key).unwrap_or_default();

        // Value-conflict lint per file; joins the diagnostics the Ctx/Plan layer surfaces.
        for warning in detect_conflicts(&body) {
            diags.push(knixl_modules::Diagnostic { span: None, message: warning });
        }
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
            raw,
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

/// Plan-time lint (docs/02): two assignments to the same option path in one file, neither
/// disambiguated by a priority, is a value conflict Nix rejects at eval and the oracle
/// cannot see. Returns one warning per offending path.
fn detect_conflicts(assignments: &[Assignment]) -> Vec<String> {
    let mut groups: BTreeMap<String, Vec<&Assignment>> = BTreeMap::new();
    for a in assignments {
        groups.entry(exact_path_key(&a.path)).or_default().push(a);
    }
    let mut warnings = Vec::new();
    for (path, group) in groups {
        let unprioritised = group.iter().filter(|a| a.priority.is_none()).count();
        if unprioritised >= 2 {
            warnings.push(format!(
                "option `{path}` is assigned {} times without a disambiguating priority",
                group.len()
            ));
        }
    }
    warnings
}

/// Exact path key (quoted segments kept distinct, unlike to_option_key's `<name>`), so a
/// conflict is only flagged for genuinely the same path.
fn exact_path_key(path: &knixl_ir::AttrPath) -> String {
    use knixl_ir::AttrKey;
    path.0
        .iter()
        .map(|k| match k {
            AttrKey::Ident(s) => s.clone(),
            AttrKey::Quoted(s) => format!("{s:?}"),
        })
        .collect::<Vec<_>>()
        .join(".")
}

fn bucket_key(bucket: &Bucket, host_name: &str) -> String {
    match bucket {
        Bucket::Default => host_name.to_string(),
        // docs/03: a named side-file is `<host>-<name>.nix`, e.g. db-backup.nix.
        Bucket::Named(name) => format!("{host_name}-{name}"),
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

#[cfg(test)]
mod tests {
    use super::*;
    use knixl_ir::{AttrKey, AttrPath, Priority};

    fn assign(path: &[&str], priority: Option<Priority>) -> Assignment {
        Assignment {
            path: AttrPath(path.iter().map(|s| AttrKey::Ident((*s).into())).collect()),
            value: NixExpr::Bool(true),
            priority,
            condition: None,
            doc: None,
        }
    }

    #[test]
    fn conflict_flagged_when_two_unprioritised_assignments_share_a_path() {
        let a = [assign(&["services", "x"], None), assign(&["services", "x"], None)];
        let w = detect_conflicts(&a);
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("services.x"));
    }

    #[test]
    fn priority_disambiguates_a_shared_path() {
        let a = [
            assign(&["services", "x"], None),
            assign(&["services", "x"], Some(Priority::Force)),
        ];
        assert!(detect_conflicts(&a).is_empty());
    }

    #[test]
    fn distinct_paths_do_not_conflict() {
        let a = [assign(&["services", "x"], None), assign(&["services", "y"], None)];
        assert!(detect_conflicts(&a).is_empty());
    }
}
