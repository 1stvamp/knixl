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
    pub path: PathBuf,         // e.g. generated/hosts/web.nix
    pub text: String,          // formatted Nix, the thing that gets hashed
    pub from: PathBuf,         // the KDL input it derived from
    pub modules: Vec<String>,  // modules that contributed (drives the lock entry)
    pub warnings: Vec<String>, // non-fatal lints: unclaimed nodes, value conflicts
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

/// Generate every output file for the given hosts, deterministically. When `oracle` is
/// Some, every emitted option path is validated against the real NixOS option set.
pub fn generate(
    hosts: &[HostSource],
    registry: &Registry,
    formatter: &Formatter,
    tool: &Version,
    oracle: Option<&knixl_oracle::Oracle>,
) -> Result<Vec<GeneratedFile>, GenerateError> {
    let mut out = Vec::new();
    for host in hosts {
        out.extend(generate_one(host, registry, formatter, tool, oracle)?);
    }
    Ok(out)
}

fn generate_one(
    host: &HostSource,
    registry: &Registry,
    formatter: &Formatter,
    tool: &Version,
    oracle: Option<&knixl_oracle::Oracle>,
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
    // Distinct modules that contributed to each output file, so the lock records honest
    // per-file attribution rather than every module on every file.
    let mut file_modules: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut diags: Vec<knixl_modules::Diagnostic> = Vec::new();

    for node in doc.nodes() {
        let name = node.name().value();
        let module = registry
            .get(name)
            .ok_or_else(|| GenerateError::UnknownNode(name.to_string()))?;

        module.schema().validate(node).map_err(|ds| {
            GenerateError::Validation(ds.into_iter().map(|d| d.message).collect())
        })?;

        let module_name = module.id().name;

        let mut ctx = LowerCtx::new(
            Scope { host: host_name.clone() },
            registry,
            &mut diags,
        );
        let mut output = module.lower(node, &mut ctx)?;
        // The top-level module claims any unit its delegates did not already attribute.
        output.attribute(&module_name);

        for unit in output.units {
            let key = bucket_key(&unit.bucket, &host_name);
            file_modules.entry(key.clone()).or_default().insert(unit.module);
            files.entry(key).or_default().push(unit.assignment);
        }
        for r in output.raw {
            let key = bucket_key(&r.bucket, &host_name);
            file_modules.entry(key.clone()).or_default().insert(r.module);
            raw_files.entry(key).or_default().push(r.raw);
        }
    }

    let module_versions = registry.module_versions();

    // Oracle: validate every emitted option path against the real NixOS option set.
    if let Some(oracle) = oracle {
        let mut errors = Vec::new();
        for body in files.values() {
            for a in body {
                if let Err(mismatch) = oracle.check(&a.path, &a.value) {
                    errors.push(format!("{mismatch:?}"));
                }
            }
        }
        if !errors.is_empty() {
            return Err(GenerateError::Validation(errors));
        }
    }

    // Every output file: any bucket that produced assignments or raw passthrough.
    let mut keys: BTreeSet<String> = files.keys().cloned().collect();
    keys.extend(raw_files.keys().cloned());

    // Named side-files (anything not the host's own file). The host file imports them.
    let side_files: Vec<String> = keys.iter().filter(|k| *k != &host_name).cloned().collect();

    // Host-level lints (e.g. an unclaimed child node) are not tied to one output file.
    // They ride on the host's own file; if the host emits only side-files, the first
    // generated file carries them instead so they are never dropped.
    let mut host_lints: Vec<String> = diags.iter().map(|d| d.message.clone()).collect();

    let mut generated = Vec::new();
    for key in &keys {
        let mut body = files.remove(key).unwrap_or_default();
        let raw = raw_files.remove(key).unwrap_or_default();

        // Value-conflict lint is per file; the host-level lints attach to the host's file.
        let mut warnings = detect_conflicts(&body);
        if *key == host_name {
            warnings.append(&mut host_lints);
        }

        // let-hoisting: dedupe repeated compound values into top-level bindings.
        let lets = knixl_ir::hoist::hoist(&mut body);
        let imports = if *key == host_name {
            side_files
                .iter()
                .map(|n| NixExpr::Path(PathBuf::from(format!("./{n}.nix"))))
                .collect()
        } else {
            Vec::new()
        };

        // Only the modules that actually contributed to this file, resolved to their
        // pinned versions for the header provenance and the lock record.
        let file_module_names: Vec<String> = file_modules
            .get(key)
            .map(|s| s.iter().filter(|n| !n.is_empty()).cloned().collect())
            .unwrap_or_default();
        let module_refs: Vec<ModuleRef> = file_module_names
            .iter()
            .map(|n| ModuleRef {
                name: n.clone(),
                version: module_versions.get(n).cloned().unwrap_or_else(|| Version::new(0, 0, 0)),
            })
            .collect();

        let module = NixModule {
            header: module_header(),
            imports,
            lets,
            body,
            raw,
            provenance: Provenance {
                tool_version: tool.clone(),
                modules: module_refs,
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
            modules: file_module_names,
            warnings,
        });
    }

    // A host that emits only side-files still gets its lints reported.
    if !host_lints.is_empty() {
        if let Some(first) = generated.first_mut() {
            first.warnings.append(&mut host_lints);
        }
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
