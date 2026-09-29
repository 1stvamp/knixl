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

use crate::flake::{
    pinned_url, render_input_flake, render_system_flake, FlakeHost, FlakeImage, FlakeInputSpec,
};
use crate::project::{parse_project, ModuleSource};
use crate::{generate, generate_image_targets, GenerateError, HostSource};

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
    /// Input mode only (ADR 0014): where the nix-owned `generated/flake.lock` disagrees with
    /// the revs `knixl.lock.kdl` pins, or is missing. Reported by `check`, never by generate,
    /// since the flake has to exist before nix can lock it.
    pub flake_lock_problems: Vec<String>,
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
    gather_with_lock(root, formatter, tool, None)
}

/// `gather`, but planning against `lock` instead of the one on disk: what `upgrade` needs once
/// it has resolved new pins in memory, so the expected output (the flake in particular) is
/// rendered from those pins rather than from the lock they are about to replace.
pub fn gather_with_lock(
    root: &Path,
    formatter: &Formatter,
    tool: Version,
    lock: Option<Lock>,
) -> Result<Project, GatherError> {
    let project = parse_project(root).map_err(|e| GatherError::Module(e.to_string()))?;
    let hosts = read_hosts(root)?;
    // Read the lock before building the registry: the fetched layer (issue #13) resolves
    // declared `modules {}` sources through the lock's pins, but a fresh project with no
    // lock yet still needs a registry (the fallback `Lock` literal below seeds `modules`
    // from it), so the lock is read once here and reused for both.
    let existing_lock = match lock {
        Some(l) => Some(l),
        None => read_lock(root)?,
    };
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
            flake_inputs: Vec::new(),
            inputs: BTreeMap::new(),
            modules: registry.module_versions(),
            outputs: Vec::new(),
            pins: BTreeMap::new(),
            baselines: BTreeMap::new(),
        },
    };

    // Hosts declaring their own `oracle-modules` override (ADR 0008; the requires-a-baseline
    // rule is enforced below, alongside the unresolved-release check).
    let declared_oracle_hosts = declared_oracle_module_hosts(&hosts);

    // Resolve each host's oracle option set (issue #22, extended by #35/ADR 0008 to the
    // augmented set). KNIXL_OPTIONS_JSON wins when set (explicit override, and what the path
    // tests use): every host maps to that one options file. Otherwise each host's rev and
    // module pins come from its own lock baseline where declared, else the project's defaults
    // (ADR 0008); a host with nothing cached for its effective set is simply absent from the
    // map, so generation proceeds without option checks for it.
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
    // Referenced sops secrets per host, for the flake's sops wiring (ADR 0014).
    let mut host_secrets: BTreeMap<String, Vec<String>> = BTreeMap::new();
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
                    if !f.secrets.is_empty() {
                        let host = f.path.file_stem().unwrap_or_default().to_string_lossy();
                        host_secrets.insert(host.into_owned(), f.secrets.clone());
                    }
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

    // Image-target module files (ADR 0012, 0013: installer ISOs and lxc guest images). Emitted
    // whether or not `system {}` is present; the build/flake output is wired only when it is.
    match generate_image_targets(
        &project.image_targets,
        &registry,
        formatter,
        &tool,
        project.secrets_backend,
    ) {
        Ok(files) => {
            for f in files {
                generated.insert(f.path.clone(), f.text.clone());
                warnings.extend(
                    f.warnings
                        .iter()
                        .map(|w| format!("{}: {w}", f.from.display())),
                );
                expected.push(ExpectedFile {
                    path: f.path,
                    hash: hash(f.text.as_bytes()),
                    from: f.from,
                    modules: f.modules,
                });
            }
        }
        Err(GenerateError::Validation(errs)) => validation_errors.extend(errs),
        Err(other) => return Err(other.into()),
    }

    // A declared baseline that is not yet resolved (no lock entry) or that has moved to a
    // different release than what is now declared, is a validation error naming the fix
    // (issue #22). Checked here rather than in `generate` because it compares declared KDL
    // state against the lock, not against the oracle's option set.
    let declared_baselines = declared_baselines(&hosts);
    let declared_revs = declared_baseline_revs(&hosts);
    for (host, rev) in &declared_revs {
        if !declared_baselines.contains_key(host) {
            validation_errors.push(format!(
                "host \"{host}\": nixpkgs rev= needs a release= beside it"
            ));
        } else if !crate::project::is_full_rev(rev) {
            validation_errors.push(format!(
                "host \"{host}\": nixpkgs rev \"{rev}\" is not a full 40-character commit"
            ));
        }
    }
    for (host, release) in &declared_baselines {
        let rev = declared_revs.get(host);
        let resolved = lock
            .baselines
            .get(host)
            .is_some_and(|b| &b.release == release && rev.is_none_or(|r| &b.nixpkgs_rev == r));
        if !resolved {
            let pinned = rev.map(|r| format!(" at {r}")).unwrap_or_default();
            validation_errors.push(format!(
                "host \"{host}\": nixpkgs release \"{release}\"{pinned} is not resolved: run knixl upgrade"
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

    // ADR 0014: an oracle module may come from a declared flake input instead of a flake ref.
    let declared_inputs: BTreeSet<&str> = project
        .system
        .iter()
        .flat_map(|s| s.inputs.iter().map(|i| i.name.as_str()))
        .collect();
    // ADR 0014: sops-nix wiring names the input its module comes from, so it needs input mode.
    if let Some(w) = &project.sops {
        if !declared_inputs.contains(w.input.as_str()) {
            validation_errors.push(format!(
                "secrets input=\"{}\" is not a flake input declared in system {{}}",
                w.input
            ));
        }
    }

    let host_overrides = hosts
        .iter()
        .filter_map(|h| crate::project::parse_host_oracle_modules(&h.src));
    for m in project
        .oracle_modules
        .iter()
        .chain(host_overrides.collect::<Vec<_>>().iter().flatten())
    {
        let Some(input) = &m.input else { continue };
        let problem = if !m.flake.is_empty() {
            "declares both flake= and input=".to_string()
        } else if input == "nixpkgs" {
            "names input `nixpkgs`, whose modules are already part of every system".to_string()
        } else if !declared_inputs.contains(input.as_str()) {
            format!("names input `{input}`, which is not declared in system {{}}")
        } else {
            continue;
        };
        validation_errors.push(format!("oracle module \"{}\" {problem}", m.name));
    }

    // Opt-in system-assembly flake (ADR 0009): every host needs a resolved baseline rev to
    // pin nixpkgs, since a partial flake would lie about the fleet.
    let mut flake_lock_problems = Vec::new();
    if let Some(system) = &project.system {
        let mut flake_hosts = Vec::new();
        let mut missing = false;
        for name in host_names(&hosts) {
            match lock.baselines.get(&name) {
                Some(b) if !b.nixpkgs_rev.is_empty() => flake_hosts.push(FlakeHost {
                    name: name.clone(),
                    baseline_rev: b.nixpkgs_rev.clone(),
                    module_path: format!("./hosts/{name}.nix"),
                    system: None,
                    input_modules: Vec::new(),
                    inline_modules: Vec::new(),
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
        // Image targets pin to the project's default nixpkgs rev (the oracle's), the single rev
        // the project validates against (ADR 0012, 0013). With no lock rev yet, the build output
        // is skipped (the module file is still generated above).
        let flake_images: Vec<FlakeImage> = if lock.oracle.nixpkgs_rev.is_empty() {
            Vec::new()
        } else {
            project
                .image_targets
                .iter()
                .map(|t| FlakeImage {
                    name: t.name.clone(),
                    baseline_rev: lock.oracle.nixpkgs_rev.clone(),
                    module_path: format!("./{}/{}.nix", t.kind.output_dir(), t.name),
                    system: t.system.clone(),
                    kind: t.kind,
                })
                .collect()
        };
        let raw = if system.inputs.is_empty() {
            (!missing).then(|| {
                render_system_flake(
                    &flake_hosts,
                    &flake_images,
                    &system.state_version,
                    &system.nixpkgs_url,
                )
            })
        } else {
            let systems = host_systems(&hosts);
            let input_modules = host_input_modules(&hosts, &project.oracle_modules);
            let sops = match &project.sops {
                Some(w) => match sops_module(w) {
                    Ok(m) => Some((w, m)),
                    Err(e) => {
                        validation_errors.push(e);
                        None
                    }
                },
                None => None,
            };
            for h in &mut flake_hosts {
                h.system = systems.get(&h.name).cloned();
                h.input_modules = input_modules.get(&h.name).cloned().unwrap_or_default();
                if let Some((w, settings)) = &sops {
                    let import = format!("inputs.\"{}\".nixosModules.\"sops\"", esc(&w.input));
                    if !h.input_modules.contains(&import) {
                        h.input_modules.push(import);
                    }
                    let mut body = settings.clone();
                    for name in host_secrets.get(&h.name).into_iter().flatten() {
                        body.push(format!("sops.secrets.\"{}\" = {{ }};", esc(name)));
                    }
                    h.inline_modules.push(format!("{{ {} }}", body.join(" ")));
                }
            }
            input_mode_flake(
                system,
                &lock,
                &flake_hosts,
                &project.image_targets,
                missing,
                &mut validation_errors,
            )
            .map(|(raw, pins)| {
                flake_lock_problems = crate::flake::flake_lock_problems(
                    std::fs::read_to_string(root.join("generated/flake.lock"))
                        .ok()
                        .as_deref(),
                    &pins,
                );
                raw
            })
        };
        // Only emit when every host resolved; a partial flake would lie about the fleet.
        if let Some(raw) = raw {
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
        flake_lock_problems,
    })
}

/// Build the input-mode flake (ADR 0014), or `None` with the reason pushed onto `errors`.
/// Every host shares the one `nixpkgs` input, so all hosts must sit on the same baseline rev;
/// every other input needs a `flake-input` pin in the lock matching its declared url (and its
/// declared rev, if any). Also returns each input's pinned rev, for the `flake.lock` check.
fn input_mode_flake(
    system: &crate::project::SystemConfig,
    lock: &Lock,
    hosts: &[FlakeHost],
    images: &[crate::project::ImageTarget],
    hosts_missing: bool,
    errors: &mut Vec<String>,
) -> Option<(String, BTreeMap<String, String>)> {
    let revs: BTreeSet<&str> = hosts.iter().map(|h| h.baseline_rev.as_str()).collect();
    let nixpkgs_rev = match revs.len() {
        0 if !lock.oracle.nixpkgs_rev.is_empty() => lock.oracle.nixpkgs_rev.clone(),
        0 => return None,
        1 => revs.iter().next().unwrap().to_string(),
        _ => {
            let found: Vec<String> = hosts
                .iter()
                .map(|h| format!("{} at {}", h.name, h.baseline_rev))
                .collect();
            errors.push(format!(
                "system {{}} with flake inputs needs every host on one nixpkgs baseline rev (found {})",
                found.join(", ")
            ));
            return None;
        }
    };

    let mut missing = hosts_missing;
    let mut specs = Vec::new();
    let mut pins = BTreeMap::new();
    for input in &system.inputs {
        let rev = if input.name == "nixpkgs" {
            Some(nixpkgs_rev.clone())
        } else {
            lock.flake_inputs
                .iter()
                .find(|p| {
                    p.name == input.name
                        && p.url == input.url
                        && input.rev.as_ref().is_none_or(|r| &p.rev == r)
                })
                .map(|p| p.rev.clone())
        };
        let Some(rev) = rev else {
            errors.push(format!(
                "flake input \"{}\" is not resolved: run knixl upgrade",
                input.name
            ));
            missing = true;
            continue;
        };
        specs.push(FlakeInputSpec {
            name: input.name.clone(),
            url: pinned_url(&input.url, &rev),
            flake: input.flake,
            follows: input.follows.clone(),
        });
        pins.insert(input.name.clone(), rev);
    }
    if missing {
        return None;
    }

    let images: Vec<FlakeImage> = images
        .iter()
        .map(|t| FlakeImage {
            name: t.name.clone(),
            baseline_rev: nixpkgs_rev.clone(),
            module_path: format!("./{}/{}.nix", t.kind.output_dir(), t.name),
            system: t.system.clone(),
            kind: t.kind,
        })
        .collect();
    let raw = render_input_flake(
        &specs,
        hosts,
        &images,
        &system.state_version,
        system.formatter.as_deref(),
    );
    Some((raw, pins))
}

/// The input-backed modules each host imports (ADR 0014): its own `oracle-modules` override if
/// it has one (replace, as ADR 0008), else the project default, in declared order.
fn host_input_modules(
    hosts: &[HostSource],
    project: &[crate::project::OracleModule],
) -> BTreeMap<String, Vec<String>> {
    hosts
        .iter()
        .filter_map(|h| {
            let name = host_names(std::slice::from_ref(h)).pop()?;
            let own = crate::project::parse_host_oracle_modules(&h.src);
            let modules = crate::project::effective_modules(project, own.as_deref())
                .iter()
                .filter_map(|m| {
                    let input = m.input.as_ref()?;
                    Some(format!(
                        "inputs.\"{}\".nixosModules.\"{}\"",
                        input.replace('\\', "\\\\").replace('"', "\\\""),
                        m.attr.replace('\\', "\\\\").replace('"', "\\\"")
                    ))
                })
                .collect();
            Some((name, modules))
        })
        .collect()
}

/// The settings half of the sops-nix wiring, as Nix assignments (ADR 0014). `default-file` is
/// written relative to knixl.kdl and rewritten relative to generated/flake.nix.
fn sops_module(w: &crate::project::SopsWiring) -> Result<Vec<String>, String> {
    let mut body = Vec::new();
    if let Some(file) = &w.default_file {
        let parts = crate::project_relative(Path::new(""), file)
            .map_err(|why| format!("secrets default-file \"{file}\": {why}"))?;
        body.push(format!("sops.defaultSopsFile = ../{};", parts.join("/")));
    }
    if !w.ssh_key_paths.is_empty() {
        let paths: Vec<String> = w
            .ssh_key_paths
            .iter()
            .map(|p| format!("\"{}\"", esc(p)))
            .collect();
        body.push(format!("sops.age.sshKeyPaths = [ {} ];", paths.join(" ")));
    }
    Ok(body)
}

/// Escape a value spliced into a Nix double-quoted string literal.
fn esc(v: &str) -> String {
    v.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace("${", "\\${")
}

/// Each host's declared `system` double, keyed by host name.
fn host_systems(hosts: &[HostSource]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for host in hosts {
        let Ok(doc) = knixl_kdl::parse(&host.src) else {
            continue;
        };
        for node in doc.nodes().iter().filter(|n| n.name().value() == "host") {
            if let (Some(name), Some(system)) = (
                crate::first_arg_str(node),
                knixl_kdl::child_arg_str(node, "system"),
            ) {
                out.insert(name, system);
            }
        }
    }
    out
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

/// Hosts that pin their baseline to an exact commit with `nixpkgs rev=".."` (ADR 0014), keyed
/// by host name, mirroring `declared_baselines`.
pub fn declared_baseline_revs(hosts: &[HostSource]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for host in hosts {
        let Ok(doc) = knixl_kdl::parse(&host.src) else {
            continue;
        };
        for node in doc.nodes().iter().filter(|n| n.name().value() == "host") {
            if let (Some(name), Some(rev)) = (
                crate::first_arg_str(node),
                knixl_kdl::child_prop_str(node, "nixpkgs", "rev"),
            ) {
                out.insert(name, rev);
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

/// Just the module registry (built-ins, local, embedded stdlib): no formatter or oracle
/// needed, so this works even without nix/nixfmt. Passes no declared sources/pins, so the
/// fetched layer (issue #13) stays empty here; that needs the lock read `gather` already does.
/// Drops shadow notices and validation errors; call `build_registry` directly for those.
pub fn registry(root: &Path) -> Result<Registry, GatherError> {
    Ok(build_registry(root, &[], &[])?.0)
}

/// Layers the registry per ADR 0010 (built-in, local, fetched, embedded stdlib); see there for
/// the precedence and shadow-notice rules. Fetched sources resolve through the lock's `pins`
/// rather than the network (issue #13), so this stays offline.
///
/// An unresolved declared source is collected in the third element rather than returned as an
/// `Err`, so every problem in a project surfaces together, mirroring the unresolved-baseline
/// check in `gather`. A cached manifest whose hash no longer matches its pin is still a hard
/// `Err`, never a silent refetch.
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

    let mut notices = Vec::new();
    let mut validation_errors = Vec::new();

    // Local project modules (highest after built-ins). A local module claiming a node a
    // built-in already claims is a shadow notice, not a hard error (ADR 0010: every
    // across-layer collision is "higher wins + a shadow notice, never silent"), mirroring
    // the fetched loop below. A duplicate WITHIN this layer (two local modules claiming the
    // same node) is still a hard error: nothing outranks it to resolve the collision.
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
            let node = module.node_name().to_string();
            if builtin_nodes.contains(&node) {
                notices.push(knixl_modules::ShadowNotice {
                    node,
                    kept: knixl_modules::ModuleLayer::Builtin,
                    shadowed: knixl_modules::ModuleLayer::Local,
                });
                continue;
            }
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

    // Fetched layer (issue #13). `pre_fetch_nodes` is everything already claimed by built-in
    // or local (the same set either way, since this snapshot sits right between the local
    // layer above and the fetched layer below) so the nodes this layer itself adds can be
    // told apart afterwards, for `register_stdlib`'s shadow attribution.
    let pre_fetch_nodes: BTreeSet<String> =
        registry.entries().map(|(k, _)| k.to_string()).collect();
    for source in sources {
        let name = source.name.as_str();
        let Some(pin) = pins.iter().find(|p| p.name == source.name) else {
            validation_errors.push(format!(
                "module source \"{name}\": not resolved (no lock pin): run knixl install or upgrade"
            ));
            continue;
        };
        let Some(cache_path) = module_cache_path(&pin.url, &pin.rev, &pin.path) else {
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
    let fetched_nodes: BTreeSet<String> = registry
        .entries()
        .map(|(k, _)| k.to_string())
        .filter(|k| !pre_fetch_nodes.contains(k))
        .collect();
    let stdlib_notices = knixl_modules::stdlib::register_stdlib(
        &mut registry,
        &builtin_nodes,
        &local_nodes,
        &fetched_nodes,
    );
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
