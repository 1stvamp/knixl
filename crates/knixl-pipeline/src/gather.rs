//! Gather a project's world for planning: parse hosts, build the registry, generate the
//! expected output, read the generated files already on disk, parse the lock, and collect
//! running versions. This is the read side of `Plan::compute`, reusable by the CLI and
//! (later) an LSP or GitHub Action. It does I/O but no writes.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use knixl_lock::model::{FormatterPin, ModuleSourcePin, OracleModulePin, OraclePin};
use knixl_lock::reconcile::{DiskState, ExpectedFile, Inputs, Versions};
use knixl_lock::Lock;
use knixl_modules::builtin::register_builtins;
use knixl_modules::template::DeclarativeModule;
use knixl_modules::{Module, Registry};
use knixl_nix::module_fetch::{hash_module, module_cache_path};
use knixl_nix::{hash, Formatter};
use semver::Version;

use crate::flake::{render_system_flake, FlakeHost};
use crate::project::{parse_project, ModuleSource};
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
    /// Per-host oracle, keyed by host name (issue #22): a host with a declared baseline is
    /// validated against its own rev's option set, one without falls back to the lock's
    /// default rev. Absent entry means best-effort skip (nothing cached for that rev).
    pub oracles: BTreeMap<String, knixl_oracle::Oracle>,
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
    let project = parse_project(root).map_err(|e| GatherError::Module(e.to_string()))?;
    let hosts = read_hosts(root)?;
    // Read the lock before building the registry: the fetched layer (issue #13) resolves
    // declared `modules {}` sources through the lock's pins, but a fresh project with no
    // lock yet still needs a registry (the fallback `Lock` literal below seeds `modules`
    // from it), so the lock is read once here and reused for both.
    let existing_lock = read_lock(root)?;
    let module_pins: &[ModuleSourcePin] = existing_lock
        .as_ref()
        .map(|l| l.module_sources.as_slice())
        .unwrap_or(&[]);
    let (registry, module_notices, module_validation_errors) =
        build_registry(root, &project.module_sources, module_pins)?;

    let formatter_pin = FormatterPin {
        name: formatter.name.clone(),
        version: formatter.version.clone(),
    };
    // No lockfile means a fresh project: seed the baseline from the running versions so
    // there is no phantom skew (skew only means a recorded version actually moved).
    let lock = match existing_lock {
        Some(l) => l,
        None => Lock {
            version: 1,
            tool: tool.clone(),
            formatter: formatter_pin.clone(),
            oracle: OraclePin {
                nixpkgs_rev: String::new(),
                options_hash: String::new(),
                modules: Vec::new(),
            },
            module_sources: Vec::new(),
            inputs: BTreeMap::new(),
            modules: registry.module_versions(),
            outputs: Vec::new(),
            pins: BTreeMap::new(),
            baselines: BTreeMap::new(),
        },
    };

    // Which hosts declare their own `oracle-modules` override (ADR 0008): a host with one
    // replaces the project default rather than falling back to it, but ONLY when it also
    // carries a baseline to store the resolved pins in (checked below, alongside the
    // unresolved-release check).
    let declared_oracle_hosts = declared_oracle_module_hosts(&hosts);

    // Resolve each host's oracle option set (issue #22, extended by #35/ADR 0008 to the
    // augmented set: nixpkgs plus declared out-of-tree module pins). KNIXL_OPTIONS_JSON wins
    // (explicit override, and the path tests use): every host maps to that one options file.
    // Otherwise each host's rev is its lock baseline if declared, else the lock's default
    // nixpkgs rev; its module pins are its own baseline's if it declares an override, else the
    // project's `oracle.modules`; the set cached for that effective (rev, pins) key validates
    // it. If nothing is cached for a host's effective set, that host is simply absent from the
    // map: generation proceeds without option checks for it (best-effort, per host).
    let names = host_names(&hosts);
    let oracles: BTreeMap<String, knixl_oracle::Oracle> = match std::env::var("KNIXL_OPTIONS_JSON")
    {
        Ok(p) => names
            .iter()
            .filter_map(|n| {
                knixl_oracle::Oracle::from_options_json(Path::new(&p))
                    .ok()
                    .map(|o| (n.clone(), o))
            })
            .collect(),
        Err(_) => names
            .iter()
            .filter_map(|n| {
                let baseline = lock.baselines.get(n);
                let rev = baseline
                    .map(|b| b.nixpkgs_rev.as_str())
                    .unwrap_or(&lock.oracle.nixpkgs_rev);
                let modules: &[OracleModulePin] = if declared_oracle_hosts.contains(n) {
                    baseline.map(|b| b.modules.as_slice()).unwrap_or(&[])
                } else {
                    &lock.oracle.modules
                };
                let tuples: Vec<(String, String, String)> = modules
                    .iter()
                    .map(|m| (m.url.clone(), m.rev.clone(), m.attr.clone()))
                    .collect();
                let path = knixl_oracle::cache_path_for(rev, &tuples)?;
                if !path.is_file() {
                    return None;
                }
                knixl_oracle::Oracle::from_options_json(&path)
                    .ok()
                    .map(|o| (n.clone(), o))
            })
            .collect(),
    };

    let mut generated: BTreeMap<PathBuf, String> = BTreeMap::new();
    // Shadowed stdlib modules are non-fatal: fold into the same warnings channel as the
    // generate-path lints, so shadowing is reported but never gates.
    let mut warnings: Vec<String> = module_notices.iter().map(|n| n.message()).collect();
    let (mut expected, mut validation_errors) = match generate(
        &hosts,
        &registry,
        formatter,
        &tool,
        &oracles,
        &lock.pins,
        project.secrets_backend,
    ) {
        Ok(files) => {
            let expected = files
                .into_iter()
                .map(|f| {
                    generated.insert(f.path.clone(), f.text.clone());
                    warnings.extend(
                        f.warnings
                            .iter()
                            .map(|w| format!("{}: {w}", f.from.display())),
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

    // A declared `modules {}` source with no matching lock pin (issue #13) is a validation
    // error naming the fix, exactly like an unresolved baseline: `build_registry` already
    // refused to register it rather than silently skipping.
    validation_errors.extend(module_validation_errors);

    // A declared baseline that is not yet resolved (no lock entry) or that has moved to a
    // different release than what is now declared, is a validation error naming the fix
    // (issue #22). Checked here rather than in `generate` because it compares declared KDL
    // state against the lock, not against the oracle's option set.
    let declared_baselines = declared_baselines(&hosts);
    for (host, release) in &declared_baselines {
        let resolved = lock
            .baselines
            .get(host)
            .is_some_and(|b| &b.release == release);
        if !resolved {
            validation_errors.push(format!(
                "host \"{host}\": nixpkgs release \"{release}\" is not resolved: run knixl upgrade"
            ));
        }
    }

    // ADR 0008: a host may declare its own `oracle-modules` override only alongside a
    // declared `nixpkgs release=` (that baseline is where the lock carries its resolved
    // pins); one with no declared release has nowhere to store them.
    for host in &declared_oracle_hosts {
        if !declared_baselines.contains_key(host) {
            validation_errors.push(format!(
                "host \"{host}\": oracle-modules requires a declared nixpkgs release"
            ));
        }
    }

    // Opt-in system-assembly flake (ADR 0009): every host needs a resolved baseline rev to
    // pin nixpkgs, since a partial flake would lie about the fleet.
    if let Some(system) = &project.system {
        let mut flake_hosts = Vec::new();
        let mut missing = false;
        for name in host_names(&hosts) {
            match lock.baselines.get(&name) {
                Some(b) if !b.nixpkgs_rev.is_empty() => flake_hosts.push(FlakeHost {
                    name: name.clone(),
                    baseline_rev: b.nixpkgs_rev.clone(),
                    module_path: format!("./hosts/{name}.nix"),
                }),
                _ => {
                    missing = true;
                    // A declared-but-unresolved release is already reported by the baseline
                    // loop above; only add the flake-specific error for a host that declares
                    // no release at all, so a single root cause is not reported twice.
                    if !declared_baselines.contains_key(&name) {
                        validation_errors.push(format!(
                            "host \"{name}\": system {{}} requires each host to declare a resolved nixpkgs baseline: run knixl install or upgrade"
                        ));
                    }
                }
            }
        }
        // Only emit when every host resolved; a partial flake would lie about the fleet.
        if !missing {
            let raw = render_system_flake(&flake_hosts, &system.state_version, &system.nixpkgs_url);
            let text = formatter
                .format(&raw)
                .map_err(|e| GatherError::Module(e.to_string()))?;
            let path = PathBuf::from("generated/flake.nix");
            generated.insert(path.clone(), text.clone());
            expected.push(ExpectedFile {
                path,
                hash: hash(text.as_bytes()),
                from: PathBuf::from("knixl.kdl"),
                modules: Vec::new(),
            });
        }
    }

    let input_hashes: BTreeMap<PathBuf, String> = hosts
        .iter()
        .map(|h| (h.path.clone(), hash(h.src.as_bytes())))
        .collect();

    let versions = Versions {
        tool,
        formatter: formatter_pin,
        oracle: lock.oracle.clone(),
        modules: registry.module_versions(),
    };

    let referenced_pins = referenced_pins(&hosts);

    Ok(Project {
        inputs: Inputs {
            expected,
            input_hashes,
            validation_errors,
            referenced_pins,
            declared_baselines: declared_baselines.into_keys().collect(),
        },
        disk: read_disk(root)?,
        lock,
        versions,
        registry,
        root: root.to_path_buf(),
        generated,
        warnings,
        oracles,
    })
}

/// Every host's own name (its `host "<name>"` positional arg, falling back to "host" the same
/// way `generate_one` does), so the oracle map is keyed exactly as `generate_one` will look it
/// up. A host that fails to parse is simply absent; `generate` will surface the parse error.
fn host_names(hosts: &[HostSource]) -> Vec<String> {
    hosts
        .iter()
        .filter_map(|h| {
            let doc = knixl_kdl::parse(&h.src).ok()?;
            let node = doc.nodes().first()?;
            Some(crate::first_arg_str(node).unwrap_or_else(|| "host".to_string()))
        })
        .collect()
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
        let Ok(doc) = knixl_kdl::parse(&host.src) else {
            continue;
        };
        for node in doc.nodes() {
            if node.name().value() != "host" {
                continue;
            }
            let Some(name) = crate::first_arg_str(node) else {
                continue;
            };
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

/// Declared per-host baseline nixpkgs release, scanned straight from the gathered KDL.
/// Keyed by the host's own name, present only for hosts with a `nixpkgs release="..."`
/// child; a host that doesn't declare one is simply absent from the map (issue #22).
pub fn declared_baselines(hosts: &[HostSource]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for host in hosts {
        let Ok(doc) = knixl_kdl::parse(&host.src) else {
            continue;
        };
        for node in doc.nodes() {
            if node.name().value() != "host" {
                continue;
            }
            let Some(name) = crate::first_arg_str(node) else {
                continue;
            };
            if let Some(release) = knixl_kdl::child_prop_str(node, "nixpkgs", "release") {
                out.insert(name, release);
            }
        }
    }
    out
}

/// Hosts that declare their own `oracle-modules` block (ADR 0008), scanned straight from the
/// gathered KDL, mirroring `declared_baselines`. Present for a host with a block even if it is
/// explicitly empty (that is still a real override, distinct from declaring no block at all);
/// a host with no block is simply absent.
pub fn declared_oracle_module_hosts(hosts: &[HostSource]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for host in hosts {
        let Ok(doc) = knixl_kdl::parse(&host.src) else {
            continue;
        };
        for node in doc.nodes() {
            if node.name().value() != "host" {
                continue;
            }
            let Some(name) = crate::first_arg_str(node) else {
                continue;
            };
            if crate::project::parse_host_oracle_modules(&host.src).is_some() {
                out.insert(name);
            }
        }
    }
    out
}

/// Build just the module registry for `root` (built-ins, local, then the embedded stdlib).
/// Unlike `gather` this needs no formatter or oracle, so listing modules works even where
/// nix/nixfmt are absent; it also passes no declared sources/pins, so the fetched layer
/// (issue #13) is empty here (that needs the lock read `gather` already does). Shadow
/// notices and validation errors are dropped; callers that need them use `build_registry`
/// directly.
pub fn registry(root: &Path) -> Result<Registry, GatherError> {
    Ok(build_registry(root, &[], &[])?.0)
}

/// Layer the registry in precedence order: built-in, then local (`<root>/modules/*`, a hard
/// error on a duplicate within this layer), then fetched (declared `modules {}` sources,
/// issue #13, resolved through the lock's `pins` rather than the network, so this stays
/// offline), then the embedded stdlib filling whatever node is still unclaimed.
///
/// A declared source with no matching pin is a validation error (never a silent skip)
/// naming `install`/`upgrade` as the fix, collected in the third element rather than
/// returned as an `Err`, so every problem in a project surfaces together (mirroring the
/// unresolved-baseline check in `gather`). A cached manifest whose hash no longer matches
/// its pin IS a hard error: the cache may be corrupt or tampered, and it must never be
/// silently refetched.
///
/// Returns `(registry, shadow notices, validation errors)`.
fn build_registry(
    root: &Path,
    sources: &[ModuleSource],
    pins: &[ModuleSourcePin],
) -> Result<(Registry, Vec<knixl_modules::ShadowNotice>, Vec<String>), GatherError> {
    let mut registry = Registry::new();
    register_builtins(&mut registry);
    let builtin_nodes: BTreeSet<String> = registry.entries().map(|(k, _)| k.to_string()).collect();

    // Local project modules (highest after built-ins). Duplicate-within-layer stays a hard error.
    let dir = root.join("modules");
    if dir.is_dir() {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();
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
            registry
                .register(Box::new(module))
                .map_err(|e| GatherError::Module(e.to_string()))?;
        }
    }
    let local_nodes: BTreeSet<String> = registry
        .entries()
        .map(|(k, _)| k.to_string())
        .filter(|k| !builtin_nodes.contains(k))
        .collect();

    // Fetched layer (issue #13).
    let mut notices = Vec::new();
    let mut validation_errors = Vec::new();
    for source in sources {
        let name = source.name.as_str();
        let Some(pin) = pins.iter().find(|p| p.name == source.name) else {
            validation_errors.push(format!(
                "module source \"{name}\": not resolved (no lock pin): run knixl install or upgrade"
            ));
            continue;
        };
        let Some(cache_path) = module_cache_path(&pin.url, &pin.rev, &source.path) else {
            validation_errors.push(format!(
                "module source \"{name}\": cannot determine a cache location (no XDG_CACHE_HOME or HOME): run knixl install or upgrade"
            ));
            continue;
        };
        if !cache_path.is_file() {
            validation_errors.push(format!(
                "module source \"{name}\": not cached: run knixl install or upgrade"
            ));
            continue;
        }
        let text = std::fs::read_to_string(&cache_path)?;
        let actual = hash_module(&text);
        if actual != pin.hash {
            let expected = &pin.hash;
            return Err(GatherError::Module(format!(
                "module source \"{name}\": cached manifest hash mismatch (expected {expected}, found {actual}): the cache may be corrupt or tampered, so it is never silently refetched; run knixl install or upgrade to refetch and re-verify"
            )));
        }
        let doc = knixl_kdl::parse(&text).map_err(|e| GatherError::Module(e.to_string()))?;
        let module = DeclarativeModule::from_kdl(&doc, &cache_path)
            .map_err(|e| GatherError::Module(e.to_string()))?;
        let node = module.node_name().to_string();
        if builtin_nodes.contains(&node) || local_nodes.contains(&node) {
            let kept = if builtin_nodes.contains(&node) {
                knixl_modules::ModuleLayer::Builtin
            } else {
                knixl_modules::ModuleLayer::Local
            };
            notices.push(knixl_modules::ShadowNotice {
                node,
                kept,
                shadowed: knixl_modules::ModuleLayer::Fetched,
            });
            continue;
        }
        // A duplicate here (neither built-in nor local already claims `node`) can only be
        // two fetched sources claiming the same node: a hard error, as within any layer.
        registry
            .register(Box::new(module))
            .map_err(|e| GatherError::Module(e.to_string()))?;
    }

    // Embedded stdlib fills any node not already claimed.
    let stdlib_notices = knixl_modules::stdlib::register_stdlib(&mut registry);
    notices.extend(stdlib_notices);
    Ok((registry, notices, validation_errors))
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
    Lock::parse(&src)
        .map(Some)
        .map_err(|e| GatherError::Lock(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declared_baselines_reads_only_declaring_hosts() {
        let hosts = vec![
            HostSource {
                path: PathBuf::from("hosts/web.kdl"),
                src:
                    "host \"web\" {\n    system \"x86_64-linux\"\n    nixpkgs release=\"25.05\"\n}"
                        .into(),
            },
            HostSource {
                path: PathBuf::from("hosts/db.kdl"),
                src: "host \"db\" {\n    system \"x86_64-linux\"\n}".into(),
            },
        ];

        let baselines = declared_baselines(&hosts);

        let expected: BTreeMap<String, String> =
            BTreeMap::from([("web".to_string(), "25.05".to_string())]);
        assert_eq!(baselines, expected);
    }
}
