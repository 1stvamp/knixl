//! Gather a project's world for planning: parse hosts, build the registry, generate the
//! expected output, read the generated files already on disk, parse the lock, and collect
//! running versions. This is the read side of `Plan::compute`, reusable by the CLI and
//! (later) an LSP or GitHub Action. It does I/O but no writes.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use knixl_lock::model::{FormatterPin, OraclePin};
use knixl_lock::reconcile::{DiskState, ExpectedFile, Inputs, Versions};
use knixl_lock::Lock;
use knixl_modules::builtin::register_builtins;
use knixl_modules::template::DeclarativeModule;
use knixl_modules::Registry;
use knixl_nix::{hash, Formatter};
use semver::Version;

use crate::{generate, GenerateError, HostSource};

/// Everything `Plan::compute` needs to reconcile a project, plus the registry (for `doc`),
/// the project root, and the freshly generated file text (for the apply path to write).
pub struct Project {
    pub inputs: Inputs,
    pub disk: DiskState,
    pub lock: Lock,
    pub versions: Versions,
    pub registry: Registry,
    pub root: PathBuf,
    pub generated: BTreeMap<PathBuf, String>,
    /// Non-fatal lints from generation (unclaimed nodes, value conflicts), each prefixed
    /// with the host source it came from. Reported but not gated on.
    pub warnings: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum GatherError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("module load: {0}")]
    Module(String),
    #[error("lockfile: {0}")]
    Lock(String),
    #[error(transparent)]
    Generate(#[from] GenerateError),
}

pub fn gather(root: &Path, formatter: &Formatter, tool: Version) -> Result<Project, GatherError> {
    let hosts = read_hosts(root)?;
    let registry = build_registry(root)?;

    let formatter_pin =
        FormatterPin { name: formatter.name.clone(), version: formatter.version.clone() };
    // No lockfile means a fresh project: seed the baseline from the running versions so
    // there is no phantom skew (skew only means a recorded version actually moved).
    let lock = match read_lock(root)? {
        Some(l) => l,
        None => Lock {
            version: 1,
            tool: tool.clone(),
            formatter: formatter_pin.clone(),
            oracle: OraclePin { nixpkgs_rev: String::new(), options_hash: String::new() },
            inputs: BTreeMap::new(),
            modules: registry.module_versions(),
            outputs: Vec::new(),
            pins: BTreeMap::new(),
        },
    };

    // Resolve the oracle option set. KNIXL_OPTIONS_JSON wins (explicit override, and the
    // path tests use). Otherwise fall back to the set cached for the lock's pinned nixpkgs
    // rev, so a `check` validates against the locked options without a manual env var. If
    // neither is present, generation proceeds without option checks (best-effort).
    let oracle = match std::env::var("KNIXL_OPTIONS_JSON") {
        Ok(p) => knixl_oracle::Oracle::from_options_json(Path::new(&p)).ok(),
        Err(_) => knixl_oracle::Oracle::from_rev_cache(&lock.oracle.nixpkgs_rev).ok().flatten(),
    };

    let mut generated: BTreeMap<PathBuf, String> = BTreeMap::new();
    let mut warnings: Vec<String> = Vec::new();
    let (expected, validation_errors) = match generate(&hosts, &registry, formatter, &tool, oracle.as_ref(), &lock.pins) {
        Ok(files) => {
            let expected = files
                .into_iter()
                .map(|f| {
                    generated.insert(f.path.clone(), f.text.clone());
                    warnings.extend(
                        f.warnings.iter().map(|w| format!("{}: {w}", f.from.display())),
                    );
                    ExpectedFile {
                        path: f.path,
                        hash: hash(f.text.as_bytes()),
                        from: f.from,
                        modules: f.modules,
                    }
                })
                .collect();
            (expected, Vec::new())
        }
        Err(GenerateError::Validation(errs)) => (Vec::new(), errs),
        Err(other) => return Err(other.into()),
    };

    let input_hashes: BTreeMap<PathBuf, String> =
        hosts.iter().map(|h| (h.path.clone(), hash(h.src.as_bytes()))).collect();

    let versions = Versions {
        tool,
        formatter: formatter_pin,
        oracle: lock.oracle.clone(),
        modules: registry.module_versions(),
    };

    let referenced_pins = referenced_pins(&hosts);

    Ok(Project {
        inputs: Inputs { expected, input_hashes, validation_errors, referenced_pins },
        disk: read_disk(root)?,
        lock,
        versions,
        registry,
        root: root.to_path_buf(),
        generated,
        warnings,
    })
}

fn read_hosts(root: &Path) -> Result<Vec<HostSource>, GatherError> {
    let dir = root.join("hosts");
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "kdl"))
        .collect();
    paths.sort();

    let mut hosts = Vec::new();
    for p in paths {
        let src = std::fs::read_to_string(&p)?;
        let path = p.strip_prefix(root).unwrap_or(&p).to_path_buf();
        hosts.push(HostSource { path, src });
    }
    Ok(hosts)
}

/// Package names declared with a versioned `package` node, per host, scanned straight
/// from the gathered KDL. Keyed by the host's own name (its `host "<name>"` positional
/// arg), with an entry for every host present, even an empty set, so a host that dropped
/// a package still prunes that pin in `build_lock_next`. A host missing from `hosts`
/// entirely is simply absent from the map, which drops all of its pins.
fn referenced_pins(hosts: &[HostSource]) -> BTreeMap<String, BTreeSet<String>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for host in hosts {
        let Ok(doc) = knixl_kdl::parse(&host.src) else { continue };
        for node in doc.nodes() {
            if node.name().value() != "host" {
                continue;
            }
            let Some(name) = crate::first_arg_str(node) else { continue };
            let set = out.entry(name).or_default();
            for child in knixl_kdl::children_named(node, "package") {
                if child.get("version").is_some() {
                    if let Some(pkg) = crate::first_arg_str(child) {
                        set.insert(pkg);
                    }
                }
            }
        }
    }
    out
}

/// Build just the module registry for `root` (built-ins plus declarative modules under
/// `modules/`). Unlike `gather` this needs no formatter or oracle, so listing modules works
/// even where nix/nixfmt are absent.
pub fn registry(root: &Path) -> Result<Registry, GatherError> {
    build_registry(root)
}

fn build_registry(root: &Path) -> Result<Registry, GatherError> {
    let mut registry = Registry::new();
    register_builtins(&mut registry);

    let dir = root.join("modules");
    if dir.is_dir() {
        let mut entries: Vec<PathBuf> =
            std::fs::read_dir(&dir)?.filter_map(|e| e.ok().map(|e| e.path())).collect();
        entries.sort();
        for entry in entries {
            let manifest = entry.join("knixl-module.kdl");
            if !manifest.exists() {
                continue;
            }
            let src = std::fs::read_to_string(&manifest)?;
            let doc = knixl_kdl::parse(&src).map_err(|e| GatherError::Module(e.to_string()))?;
            let module = DeclarativeModule::from_kdl(&doc, &manifest)
                .map_err(|e| GatherError::Module(e.to_string()))?;
            registry.register(Box::new(module)).map_err(|e| GatherError::Module(e.to_string()))?;
        }
    }
    Ok(registry)
}

fn read_disk(root: &Path) -> Result<DiskState, GatherError> {
    let mut files = BTreeMap::new();
    let dir = root.join("generated");
    if dir.is_dir() {
        collect_generated(&dir, root, &mut files)?;
    }
    Ok(DiskState { files })
}

fn collect_generated(
    dir: &Path,
    root: &Path,
    files: &mut BTreeMap<PathBuf, String>,
) -> Result<(), GatherError> {
    for entry in std::fs::read_dir(dir)?.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            collect_generated(&path, root, files)?;
        } else if path.extension().is_some_and(|x| x == "nix") {
            let content = std::fs::read_to_string(&path)?;
            // Only knixl-generated files carry the header; hand-written .nix are ignored.
            if content.contains("# Generated by knixl") {
                let rel = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
                files.insert(rel, hash(content.as_bytes()));
            }
        }
    }
    Ok(())
}

fn read_lock(root: &Path) -> Result<Option<Lock>, GatherError> {
    let path = root.join("knixl.lock.kdl");
    if !path.exists() {
        return Ok(None);
    }
    let src = std::fs::read_to_string(&path)?;
    Lock::parse(&src).map(Some).map_err(|e| GatherError::Lock(e.to_string()))
}
