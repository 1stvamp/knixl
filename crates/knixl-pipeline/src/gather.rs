//! Gather a project's world for planning: parse hosts, build the registry, generate the
//! expected output, read the generated files already on disk, parse the lock, and collect
//! running versions. This is the read side of `Plan::compute`, reusable by the CLI and
//! (later) an LSP or GitHub Action. It does I/O but no writes.

use std::collections::BTreeMap;
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

    // Generate expected output. A schema/validation error is not fatal to planning: it
    // becomes the plan's validation_errors, which the verdict maps to the Validation code.
    let mut generated: BTreeMap<PathBuf, String> = BTreeMap::new();
    let (expected, validation_errors) = match generate(&hosts, &registry, formatter, &tool) {
        Ok(files) => {
            let expected = files
                .into_iter()
                .map(|f| {
                    generated.insert(f.path.clone(), f.text.clone());
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

    let lock = read_lock(root)?;
    let versions = Versions {
        tool,
        formatter: FormatterPin { name: formatter.name.clone(), version: formatter.version.clone() },
        oracle: lock.oracle.clone(),
        modules: registry.module_versions(),
    };

    Ok(Project {
        inputs: Inputs { expected, input_hashes, validation_errors },
        disk: read_disk(root)?,
        lock,
        versions,
        registry,
        root: root.to_path_buf(),
        generated,
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

fn read_lock(root: &Path) -> Result<Lock, GatherError> {
    let path = root.join("knixl.lock.kdl");
    if !path.exists() {
        return Ok(empty_lock());
    }
    let src = std::fs::read_to_string(&path)?;
    Lock::parse(&src).map_err(|e| GatherError::Lock(e.to_string()))
}

fn empty_lock() -> Lock {
    Lock {
        version: 1,
        tool: Version::new(0, 0, 0),
        formatter: FormatterPin { name: String::new(), version: String::new() },
        oracle: OraclePin { nixpkgs_rev: String::new(), options_hash: String::new() },
        inputs: BTreeMap::new(),
        modules: BTreeMap::new(),
        outputs: Vec::new(),
    }
}
