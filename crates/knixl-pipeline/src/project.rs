//! Parsing for the project-level `knixl.kdl` file: the default nixpkgs release and the
//! project's default `oracle-modules` set. Pure parsing; nothing here is wired into the
//! pipeline yet, later tasks read `ProjectConfig`/`OracleModule` to feed the oracle.

use std::path::Path;

use kdl::{KdlDocument, KdlNode};

use knixl_kdl::children_named;

/// One oracle module reference: a flake to pull a NixOS module from, and which attr of
/// it to use (defaults to `"default"` when the KDL omits `attr=`).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct OracleModule {
    pub name: String,
    pub flake: String,
    pub attr: String,
}

/// Parsed contents of `knixl.kdl`: the default nixpkgs release (if pinned), the
/// project's default `oracle-modules` set, in source order, and the optional `system {}`
/// opt-in for emitting a bootable system flake.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct ProjectConfig {
    pub default_release: Option<String>,
    pub oracle_modules: Vec<OracleModule>,
    pub system: Option<SystemConfig>,
}

/// The default nixpkgs flake reference used when a `system {}` block omits `nixpkgs-url`.
pub const DEFAULT_NIXPKGS_URL: &str = "https://github.com/NixOS/nixpkgs";

/// Parsed `system {}` block: opts a project into emitting a bootable system flake.
/// `state_version` is mandatory (NixOS requires it and refuses to guess it for you);
/// `nixpkgs_url` defaults to `DEFAULT_NIXPKGS_URL` when the block omits it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SystemConfig {
    pub state_version: String,
    pub nixpkgs_url: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    #[error("failed to read {0}")]
    Io(std::path::PathBuf, #[source] std::io::Error),
    #[error(transparent)]
    Kdl(#[from] kdl::KdlError),
    #[error("knixl.kdl: system {{}} block requires a state-version")]
    MissingStateVersion,
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
            let nixpkgs_url = knixl_kdl::child_arg_str(node, "nixpkgs-url")
                .unwrap_or_else(|| DEFAULT_NIXPKGS_URL.to_string());
            Some(SystemConfig {
                state_version,
                nixpkgs_url,
            })
        }
    };

    Ok(ProjectConfig {
        default_release,
        oracle_modules,
        system,
    })
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
        }];
        let host = vec![OracleModule {
            name: "sops-nix".into(),
            flake: "b".into(),
            attr: "default".into(),
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
}
