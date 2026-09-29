//! Parsing for the project-level `knixl.kdl` file into `ProjectConfig`, which `gather` reads
//! to build the registry, the oracle, and the optional system flake.

use std::path::Path;

use kdl::{KdlDocument, KdlNode};

use knixl_kdl::children_named;

/// One oracle module reference: a flake to pull a NixOS module from, and which attr of
/// it to use (defaults to `"default"` when the KDL omits `attr=`). With `input` set (ADR 0014)
/// the module comes from that declared flake input, `flake` is empty, and the generated flake
/// imports it as well as the oracle validating against it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct OracleModule {
    pub name: String,
    pub flake: String,
    pub attr: String,
    pub input: Option<String>,
}

/// A declared external declarative-module source (issue #13): a flake ref plus the directory
/// within it holding `knixl-module.kdl` (empty = repo root). `name` is the local handle.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ModuleSource {
    pub name: String,
    pub flake: String,
    pub path: String,
}

/// `secrets backend="sops-nix" input="<name>" { default-file ".."; ssh-key-paths ".." }`
/// (ADR 0014): the flake imports sops-nix from `input` into every host and sets these, along
/// with a `sops.secrets` entry per referenced secret. `default_file` is relative to knixl.kdl.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SopsWiring {
    pub input: String,
    pub default_file: Option<String>,
    pub ssh_key_paths: Vec<String>,
}

/// Parsed contents of `knixl.kdl`.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct ProjectConfig {
    pub default_release: Option<String>,
    pub oracle_modules: Vec<OracleModule>,
    pub system: Option<SystemConfig>,
    pub secrets_backend: knixl_modules::SecretsBackend,
    pub sops: Option<SopsWiring>,
    pub module_sources: Vec<ModuleSource>,
    pub image_targets: Vec<ImageTarget>,
}

/// The kind of image a target builds (ADR 0012, 0013): a bootable installer ISO, or a NixOS
/// system built as an lxc image for Incus. They share one generation and flake path, differing
/// only in the base module imported and the flake output shape.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ImageKind {
    /// `installer "<name>"` -> installation-cd base, `<name>-iso` output (ADR 0012).
    Installer,
    /// `guest-image "<name>"` -> lxc-container base, `<name>-lxc`/`-lxc-metadata` outputs (ADR 0013).
    GuestLxc,
}

impl ImageKind {
    /// The KDL top-level node name that declares this kind.
    pub fn node_name(self) -> &'static str {
        match self {
            ImageKind::Installer => "installer",
            ImageKind::GuestLxc => "guest-image",
        }
    }

    /// The nixpkgs module imported (relative to `modulesPath`) ahead of the lowered tree.
    pub fn base_import(self) -> &'static str {
        match self {
            ImageKind::Installer => "installer/cd-dvd/installation-cd-minimal.nix",
            ImageKind::GuestLxc => "virtualisation/lxc-container.nix",
        }
    }

    /// The `generated/<dir>/<name>.nix` subdirectory the module file is written to.
    pub fn output_dir(self) -> &'static str {
        match self {
            ImageKind::Installer => "installer",
            ImageKind::GuestLxc => "guest-image",
        }
    }
}

/// A declared image target: `<kind-node> "<name>" [system="<double>"] { <modules> }` in
/// `knixl.kdl`. The node's children are ordinary knixl module nodes, lowered into a generated
/// module (importing the kind's base). `system` defaults to x86_64-linux.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ImageTarget {
    pub name: String,
    pub system: String,
    pub kind: ImageKind,
    pub node: KdlNode,
}

/// The default nixpkgs flake reference used when a `system {}` block omits `nixpkgs-url`.
pub const DEFAULT_NIXPKGS_URL: &str = "https://github.com/NixOS/nixpkgs";

/// Parsed `system {}` block: opts a project into emitting a bootable system flake.
/// `state_version` is mandatory (NixOS requires it and refuses to guess it for you);
/// `nixpkgs_url` defaults to `DEFAULT_NIXPKGS_URL` when the block omits it. A non-empty
/// `inputs` switches the flake to input mode (ADR 0014), where `nixpkgs_url` is unused.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SystemConfig {
    pub state_version: String,
    pub nixpkgs_url: String,
    pub inputs: Vec<FlakeInput>,
    pub formatter: Option<String>,
}

/// One `input "<name>" url="<ref>" [rev="<commit>"] [flake=#false] { follows <k>="<v>" }`
/// in `system {}` (ADR 0014). `follows` maps an input of this input to a project input.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FlakeInput {
    pub name: String,
    pub url: String,
    pub rev: Option<String>,
    pub flake: bool,
    pub follows: Vec<(String, String)>,
}

/// Whether `rev` is a full 40-character hex commit, the only form knixl pins by.
pub fn is_full_rev(rev: &str) -> bool {
    rev.len() == 40 && rev.bytes().all(|b| b.is_ascii_hexdigit())
}

#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    #[error("failed to read {0}")]
    Io(std::path::PathBuf, #[source] std::io::Error),
    #[error(transparent)]
    Kdl(#[from] kdl::KdlError),
    #[error("knixl.kdl: system {{}} block requires a state-version")]
    MissingStateVersion,
    #[error("knixl.kdl: unknown secrets backend `{0}` (expected `sops-nix` or `agenix`)")]
    UnknownSecretsBackend(String),
    #[error("knixl.kdl: modules {{}} block: module `{0}` requires a flake")]
    MissingModuleFlake(String),
    #[error("knixl.kdl: system {{}}: {0}")]
    InvalidInput(String),
}

/// Parse `root/knixl.kdl`. An absent file is not an error: it yields `ProjectConfig::default()`
/// (no pinned release, no project-wide oracle modules), since the project file is optional.
pub fn parse_project(root: &Path) -> Result<ProjectConfig, ProjectError> {
    let path = root.join("knixl.kdl");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(ProjectConfig::default()),
        Err(e) => return Err(ProjectError::Io(path, e)),
    };
    let doc: KdlDocument = text.parse()?;

    let default_release = doc
        .nodes()
        .iter()
        .find(|n| n.name().value() == "nixpkgs")
        .and_then(|n| n.get("release"))
        .and_then(|v| v.as_string())
        .map(str::to_string);

    let oracle_modules = doc
        .nodes()
        .iter()
        .find(|n| n.name().value() == "oracle-modules")
        .map(oracle_modules_from_node)
        .unwrap_or_default();

    let system = match doc.nodes().iter().find(|n| n.name().value() == "system") {
        None => None,
        Some(node) => {
            let state_version = knixl_kdl::child_arg_str(node, "state-version")
                .ok_or(ProjectError::MissingStateVersion)?;
            let declared_url = knixl_kdl::child_arg_str(node, "nixpkgs-url");
            let inputs = flake_inputs_from_node(node)?;
            if !inputs.is_empty() && declared_url.is_some() {
                return Err(ProjectError::InvalidInput(
                    "`nixpkgs-url` and `input` nodes cannot both be declared: declare nixpkgs as `input \"nixpkgs\"`".into(),
                ));
            }
            let formatter = knixl_kdl::child_arg_str(node, "formatter");
            if formatter.is_some() && inputs.is_empty() {
                return Err(ProjectError::InvalidInput(
                    "`formatter` needs flake inputs (it reads nixpkgs.legacyPackages)".into(),
                ));
            }
            Some(SystemConfig {
                state_version,
                nixpkgs_url: declared_url.unwrap_or_else(|| DEFAULT_NIXPKGS_URL.to_string()),
                inputs,
                formatter,
            })
        }
    };

    let secrets_backend = match doc
        .nodes()
        .iter()
        .find(|n| n.name().value() == "secrets")
        .and_then(|n| n.get("backend"))
    {
        None => knixl_modules::SecretsBackend::SopsNix,
        Some(v) => match v.as_string() {
            Some("sops-nix") => knixl_modules::SecretsBackend::SopsNix,
            Some("agenix") => knixl_modules::SecretsBackend::Agenix,
            Some(other) => return Err(ProjectError::UnknownSecretsBackend(other.to_string())),
            // A non-string `backend=` (e.g. `backend=5`) is malformed, not "no backend given",
            // so it must error rather than silently default to sops-nix.
            None => return Err(ProjectError::UnknownSecretsBackend(format!("{v:?}"))),
        },
    };

    let sops = match doc.nodes().iter().find(|n| n.name().value() == "secrets") {
        Some(node) => sops_wiring(node, secrets_backend)?,
        None => None,
    };

    let module_sources = match doc.nodes().iter().find(|n| n.name().value() == "modules") {
        None => Vec::new(),
        Some(node) => module_sources_from_node(node)?,
    };

    let mut image_targets = Vec::new();
    for kind in [ImageKind::Installer, ImageKind::GuestLxc] {
        for n in doc
            .nodes()
            .iter()
            .filter(|n| n.name().value() == kind.node_name())
        {
            image_targets.push(ImageTarget {
                name: knixl_kdl::first_arg_str(n).unwrap_or_default(),
                system: n
                    .get("system")
                    .and_then(|v| v.as_string())
                    .unwrap_or("x86_64-linux")
                    .to_string(),
                kind,
                node: n.clone(),
            });
        }
    }

    Ok(ProjectConfig {
        default_release,
        oracle_modules,
        system,
        secrets_backend,
        sops,
        module_sources,
        image_targets,
    })
}

fn sops_wiring(
    node: &KdlNode,
    backend: knixl_modules::SecretsBackend,
) -> Result<Option<SopsWiring>, ProjectError> {
    let bad = |m: &str| ProjectError::InvalidInput(format!("secrets: {m}"));
    let Some(input) = node.get("input").and_then(|v| v.as_string()) else {
        return Ok(None);
    };
    if backend != knixl_modules::SecretsBackend::SopsNix {
        return Err(bad("input= is only supported with backend=\"sops-nix\""));
    }
    let default_file = knixl_kdl::child_arg_str(node, "default-file");
    if let Some(f) = &default_file {
        if f.starts_with('/')
            || !f
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+' | '/'))
        {
            return Err(bad(
                "default-file must be a relative path of letters, digits, `.`, `_`, `-`, `+` and `/`",
            ));
        }
    }
    let ssh_key_paths = children_named(node, "ssh-key-paths")
        .flat_map(|n| n.entries().iter())
        .filter(|e| e.name().is_none())
        .filter_map(|e| e.value().as_string().map(str::to_string))
        .collect();
    Ok(Some(SopsWiring {
        input: input.to_string(),
        default_file,
        ssh_key_paths,
    }))
}

/// The effective module set for a host: its own `oracle-modules` block (replace) if
/// present, else the project default. `host_modules` is `None` when the host declares
/// no block at all (as opposed to an explicit empty one).
pub fn effective_modules<'a>(
    project: &'a [OracleModule],
    host_modules: Option<&'a [OracleModule]>,
) -> &'a [OracleModule] {
    host_modules.unwrap_or(project)
}

/// Read a host KDL source's own `oracle-modules` block, if it declares one. `None` means
/// the host has no block at all, so `effective_modules` should fall back to the project
/// default; that is distinct from an explicit, empty block.
pub fn parse_host_oracle_modules(host_src: &str) -> Option<Vec<OracleModule>> {
    let doc: KdlDocument = host_src.parse().ok()?;
    let host = doc.nodes().iter().find(|n| n.name().value() == "host")?;
    let block = children_named(host, "oracle-modules").next()?;
    Some(oracle_modules_from_node(block))
}

/// The `module` children of an `oracle-modules` block: `name` is the first positional
/// argument, `flake` and `attr` are props (`attr` defaults to `"default"`).
fn oracle_modules_from_node(node: &KdlNode) -> Vec<OracleModule> {
    children_named(node, "module")
        .map(|m| OracleModule {
            name: knixl_kdl::first_arg_str(m).unwrap_or_default(),
            flake: m
                .get("flake")
                .and_then(|v| v.as_string())
                .unwrap_or_default()
                .to_string(),
            attr: m
                .get("attr")
                .and_then(|v| v.as_string())
                .unwrap_or("default")
                .to_string(),
            input: m
                .get("input")
                .and_then(|v| v.as_string())
                .map(str::to_string),
        })
        .collect()
}

/// The `input` children of `system {}`, checked so the generated flake can pin each one: a
/// `nixpkgs` input is required once any is declared (its rev is the host baseline, so it
/// takes no `rev=` and no ref in its url), a `rev=` must be a full commit, and a `github:` url
/// may not carry a branch or tag (the resolver would pin HEAD, not that ref).
fn flake_inputs_from_node(node: &KdlNode) -> Result<Vec<FlakeInput>, ProjectError> {
    let bad = |m: String| ProjectError::InvalidInput(m);
    let mut inputs: Vec<FlakeInput> = Vec::new();
    for n in children_named(node, "input") {
        let name = knixl_kdl::first_arg_str(n).unwrap_or_default();
        if name.is_empty() {
            return Err(bad("an `input` needs a name".into()));
        }
        if inputs.iter().any(|i| i.name == name) {
            return Err(bad(format!("input `{name}` is declared twice")));
        }
        let url = n
            .get("url")
            .and_then(|v| v.as_string())
            .ok_or_else(|| bad(format!("input `{name}` requires a url")))?
            .to_string();
        let rev = n.get("rev").and_then(|v| v.as_string()).map(str::to_string);
        if let Some(r) = &rev {
            if !is_full_rev(r) {
                return Err(bad(format!(
                    "input `{name}`: rev \"{r}\" is not a full 40-character commit"
                )));
            }
        }
        if let Some(path) = url.strip_prefix("github:") {
            if path.split('?').next().unwrap_or("").split('/').count() != 2 {
                return Err(bad(format!(
                    "input `{name}`: url \"{url}\" carries a ref; pin it with rev= instead"
                )));
            }
        }
        if name == "nixpkgs" && rev.is_some() {
            return Err(bad(
                "input `nixpkgs` takes its rev from the host baseline: use `nixpkgs release=\"..\" rev=\"..\"` on the host".into(),
            ));
        }
        let flake = n.get("flake").and_then(|v| v.as_bool()).unwrap_or(true);
        let mut follows: Vec<(String, String)> = children_named(n, "follows")
            .flat_map(|f| f.entries().iter())
            .filter_map(|e| {
                let key = e.name()?.value().to_string();
                let value = e.value().as_string()?.to_string();
                Some((key, value))
            })
            .collect();
        follows.sort();
        inputs.push(FlakeInput {
            name,
            url,
            rev,
            flake,
            follows,
        });
    }
    if !inputs.is_empty() && !inputs.iter().any(|i| i.name == "nixpkgs") {
        return Err(bad("flake inputs need an `input \"nixpkgs\"`".into()));
    }
    for i in &inputs {
        for (_, target) in &i.follows {
            if !inputs.iter().any(|j| &j.name == target) {
                return Err(bad(format!(
                    "input `{}` follows `{target}`, which is not a declared input",
                    i.name
                )));
            }
        }
    }
    inputs.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(inputs)
}

/// The `module` children of a `modules` block: `name` is the first positional argument,
/// `flake` is a required prop (a `module` with none is a `ProjectError`), `path` is an
/// optional prop defaulting to `""` (repo root).
fn module_sources_from_node(node: &KdlNode) -> Result<Vec<ModuleSource>, ProjectError> {
    children_named(node, "module")
        .map(|m| {
            let name = knixl_kdl::first_arg_str(m).unwrap_or_default();
            let flake = m
                .get("flake")
                .and_then(|v| v.as_string())
                .ok_or_else(|| ProjectError::MissingModuleFlake(name.clone()))?
                .to_string();
            let path = m
                .get("path")
                .and_then(|v| v.as_string())
                .unwrap_or_default()
                .to_string();
            Ok(ModuleSource { name, flake, path })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_project_default_release_and_modules() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("knixl.kdl"),
            "nixpkgs release=\"25.05\"\noracle-modules {\n    module \"disko\" flake=\"github:nix-community/disko\"\n    module \"sops-nix\" flake=\"github:Mic92/sops-nix\" attr=\"default\"\n}\n").unwrap();
        let p = parse_project(dir.path()).unwrap();
        assert_eq!(p.default_release.as_deref(), Some("25.05"));
        assert_eq!(p.oracle_modules.len(), 2);
        assert_eq!(p.oracle_modules[0].name, "disko");
        assert_eq!(p.oracle_modules[0].flake, "github:nix-community/disko");
        assert_eq!(p.oracle_modules[0].attr, "default"); // defaulted
    }

    #[test]
    fn absent_project_file_is_default() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(parse_project(dir.path()).unwrap(), ProjectConfig::default());
    }

    #[test]
    fn host_oracle_modules_replace_the_project_default() {
        let project = vec![OracleModule {
            name: "disko".into(),
            flake: "a".into(),
            attr: "default".into(),
            input: None,
        }];
        let host = vec![OracleModule {
            name: "sops-nix".into(),
            flake: "b".into(),
            attr: "default".into(),
            input: None,
        }];
        // host present => host wins (replace)
        assert_eq!(effective_modules(&project, Some(&host)), host.as_slice());
        // host absent => project default
        assert_eq!(effective_modules(&project, None), project.as_slice());
    }

    #[test]
    fn parse_host_oracle_modules_reads_a_block_or_none() {
        let with = "host \"nas\" {\n    oracle-modules { module \"disko\" flake=\"x\" }\n}";
        let got = parse_host_oracle_modules(with).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "disko");
        assert!(parse_host_oracle_modules("host \"web\" { }").is_none());
    }

    #[test]
    fn parses_system_block() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("knixl.kdl"),
            "system {\n    state-version \"25.05\"\n}\n",
        )
        .unwrap();
        let p = parse_project(dir.path()).unwrap();
        let s = p.system.expect("system present");
        assert_eq!(s.state_version, "25.05");
        assert_eq!(s.nixpkgs_url, DEFAULT_NIXPKGS_URL);
    }

    fn system_err(body: &str) -> String {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("knixl.kdl"),
            format!("system {{\n    state-version \"25.11\"\n{body}\n}}\n"),
        )
        .unwrap();
        parse_project(dir.path()).unwrap_err().to_string()
    }

    #[test]
    fn system_block_reads_flake_inputs() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("knixl.kdl"),
            "system {\n    state-version \"25.11\"\n    formatter \"nixfmt-rfc-style\"\n    input \"nixpkgs\" url=\"github:NixOS/nixpkgs\"\n    input \"disko\" url=\"github:nix-community/disko\" rev=\"ff8702b4de27f72b4c78573dfb89ec74e36abdf1\" {\n        follows nixpkgs=\"nixpkgs\"\n    }\n    input \"blobs\" url=\"github:o/blobs\" flake=#false\n}\n",
        )
        .unwrap();
        let s = parse_project(dir.path()).unwrap().system.unwrap();
        let names: Vec<&str> = s.inputs.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["blobs", "disko", "nixpkgs"], "sorted by name");
        assert!(!s.inputs[0].flake);
        assert_eq!(
            s.inputs[1].follows,
            vec![("nixpkgs".to_string(), "nixpkgs".to_string())]
        );
        assert_eq!(
            s.inputs[1].rev.as_deref(),
            Some("ff8702b4de27f72b4c78573dfb89ec74e36abdf1")
        );
        assert_eq!(s.formatter.as_deref(), Some("nixfmt-rfc-style"));
    }

    #[test]
    fn flake_inputs_are_refused_when_malformed() {
        let nixpkgs = "    input \"nixpkgs\" url=\"github:NixOS/nixpkgs\"";
        for (body, needle) in [
            ("    input \"disko\" url=\"github:nix-community/disko\"".to_string(), "need an `input \"nixpkgs\"`"),
            (format!("{nixpkgs}\n    nixpkgs-url \"https://x\""), "cannot both"),
            ("    input \"nixpkgs\" url=\"github:NixOS/nixpkgs/nixos-unstable\"".to_string(), "carries a ref"),
            (format!("{nixpkgs}\n    input \"d\" url=\"github:o/d\" rev=\"abc\""), "full 40-character"),
            (format!("{nixpkgs}\n    input \"d\" url=\"github:o/d\" {{\n        follows nixpkgs=\"nope\"\n    }}"), "not a declared input"),
            (format!("{nixpkgs}\n{nixpkgs}"), "declared twice"),
            ("    formatter \"nixfmt\"".to_string(), "needs flake inputs"),
        ] {
            let err = system_err(&body);
            assert!(err.contains(needle), "{body}: {err}");
        }
    }

    #[test]
    fn secrets_input_reads_sops_wiring() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("knixl.kdl"),
            "secrets backend=\"sops-nix\" input=\"sops-nix\" {\n    default-file \"secrets/type40.yaml\"\n    ssh-key-paths \"/etc/ssh/ssh_host_ed25519_key\"\n}\n",
        )
        .unwrap();
        let sops = parse_project(dir.path()).unwrap().sops.unwrap();
        assert_eq!(sops.input, "sops-nix");
        assert_eq!(sops.default_file.as_deref(), Some("secrets/type40.yaml"));
        assert_eq!(sops.ssh_key_paths, ["/etc/ssh/ssh_host_ed25519_key"]);
    }

    #[test]
    fn secrets_without_input_has_no_wiring_and_agenix_input_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("knixl.kdl"),
            "secrets backend=\"sops-nix\"\n",
        )
        .unwrap();
        assert!(parse_project(dir.path()).unwrap().sops.is_none());
        std::fs::write(
            dir.path().join("knixl.kdl"),
            "secrets backend=\"agenix\" input=\"agenix\"\n",
        )
        .unwrap();
        let err = parse_project(dir.path()).unwrap_err().to_string();
        assert!(err.contains("only supported with backend"), "{err}");
    }

    #[test]
    fn system_block_reads_custom_nixpkgs_url() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("knixl.kdl"),
            "system {\n    state-version \"24.11\"\n    nixpkgs-url \"https://example.com/nixpkgs\"\n}\n").unwrap();
        let s = parse_project(dir.path()).unwrap().system.unwrap();
        assert_eq!(s.nixpkgs_url, "https://example.com/nixpkgs");
    }

    #[test]
    fn system_block_without_state_version_errors() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("knixl.kdl"), "system {\n}\n").unwrap();
        let err = parse_project(dir.path()).unwrap_err();
        assert!(format!("{err}").contains("state-version"), "got: {err}");
    }

    #[test]
    fn no_system_block_is_none() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("knixl.kdl"), "nixpkgs release=\"25.05\"\n").unwrap();
        assert!(parse_project(dir.path()).unwrap().system.is_none());
    }

    #[test]
    fn secrets_backend_defaults_to_sops() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("knixl.kdl"), "nixpkgs release=\"25.05\"\n").unwrap();
        let cfg = parse_project(dir.path()).unwrap();
        assert_eq!(cfg.secrets_backend, knixl_modules::SecretsBackend::SopsNix);
    }

    #[test]
    fn secrets_backend_agenix_parses() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("knixl.kdl"), "secrets backend=\"agenix\"\n").unwrap();
        let cfg = parse_project(dir.path()).unwrap();
        assert_eq!(cfg.secrets_backend, knixl_modules::SecretsBackend::Agenix);
    }

    #[test]
    fn secrets_backend_unknown_errors() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("knixl.kdl"), "secrets backend=\"vault\"\n").unwrap();
        assert!(parse_project(dir.path()).is_err());
    }

    #[test]
    fn secrets_backend_non_string_errors() {
        // A malformed `backend=` (not a string) must be a hard error, not a silent fallback
        // to sops-nix.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("knixl.kdl"), "secrets backend=5\n").unwrap();
        assert!(parse_project(dir.path()).is_err());
    }

    #[test]
    fn parses_module_sources() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("knixl.kdl"),
            "modules {\n    module \"nginx\" flake=\"github:org/knixl-nginx\"\n    module \"graf\" flake=\"github:org/g\" path=\"modules/graf\"\n}\n").unwrap();
        let p = parse_project(dir.path()).unwrap();
        assert_eq!(p.module_sources.len(), 2);
        assert_eq!(p.module_sources[0].name, "nginx");
        assert_eq!(p.module_sources[0].flake, "github:org/knixl-nginx");
        assert_eq!(p.module_sources[0].path, ""); // defaulted
        assert_eq!(p.module_sources[1].name, "graf");
        assert_eq!(p.module_sources[1].flake, "github:org/g");
        assert_eq!(p.module_sources[1].path, "modules/graf");
    }

    #[test]
    fn absent_modules_block_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("knixl.kdl"), "nixpkgs release=\"25.05\"\n").unwrap();
        assert!(parse_project(dir.path()).unwrap().module_sources.is_empty());
    }

    #[test]
    fn module_source_without_flake_errors() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("knixl.kdl"),
            "modules {\n    module \"nginx\"\n}\n",
        )
        .unwrap();
        assert!(parse_project(dir.path()).is_err());
    }
}
