//! ADR 0008 / issue #35: `gather` must load each host's oracle from the CACHE KEYED BY ITS
//! EFFECTIVE SET (nixpkgs rev plus module pins), not just its rev. A host that declares its
//! own `oracle-modules` override validates against its own resolved module pins (from its
//! lock `baseline`); a host with no override falls back to the project's `oracle.modules`.
//! Own test binary (like `oracle_per_host.rs`) so the `XDG_CACHE_HOME` env var it sets does
//! not race other test binaries' cache lookups.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use knixl_ir::{AttrKey, AttrPath, NixExpr};
use knixl_lock::model::{FormatterPin, HostBaseline, OracleModulePin, OraclePin};
use knixl_lock::Lock;
use knixl_nix::Formatter;
use knixl_pipeline::gather::gather;

/// Serializes this file's two tests, both of which set the process-global `XDG_CACHE_HOME`
/// env var: cargo runs `#[test]` fns within one binary concurrently by default, so without
/// this the two would race each other's cache directory (mirrors the `ENV_LOCK` pattern used
/// for the same reason in `crates/knixl/src/main.rs`'s tests).
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn ident_path(segs: &[&str]) -> AttrPath {
    AttrPath(
        segs.iter()
            .map(|s| AttrKey::Ident((*s).to_string()))
            .collect(),
    )
}

fn identity_formatter() -> Formatter {
    Formatter {
        name: "identity".into(),
        version: "0".into(),
        bin: PathBuf::from("cat"),
    }
}

/// Seed a fixture `options.json` at the effective-set cache path for `rev` plus `modules`
/// (the same `(url, rev, attr)` tuple shape `cache_path_for` takes), knowing only
/// `known_option`.
fn seed_effective_cache(rev: &str, modules: &[(String, String, String)], known_option: &str) {
    let dest = knixl_oracle::cache_path_for(rev, modules).expect("cache path under XDG_CACHE_HOME");
    fs::create_dir_all(dest.parent().unwrap()).unwrap();
    fs::write(
        &dest,
        format!(r#"{{ "{known_option}": {{ "type": "boolean" }} }}"#),
    )
    .unwrap();
}

#[test]
fn gather_selects_each_hosts_effective_module_set_for_its_oracle() {
    let _guard = ENV_LOCK.lock().unwrap();
    let tmp = std::env::temp_dir().join(format!(
        "knixl-oracle-module-effective-set-cache-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&tmp);
    std::env::set_var("XDG_CACHE_HOME", &tmp);

    let disko_a = (
        "https://github.com/nix-community/disko".to_string(),
        "modrev-a".to_string(),
        "default".to_string(),
    );
    let disko_b = (
        "https://github.com/nix-community/disko".to_string(),
        "modrev-b".to_string(),
        "default".to_string(),
    );

    // Host "a"'s own effective set (its baseline rev plus its own resolved module override)
    // knows disko.a.enable. The BASE (no-modules) cache for the same rev is seeded with a
    // different known option, so a test that regressed to `cache_path` (rev-only) instead of
    // `cache_path_for` (effective-set) would fail here, not pass by coincidence.
    seed_effective_cache("rev-a", &[disko_a.clone()], "disko.a.enable");
    seed_effective_cache("rev-a", &[], "system.stateVersion");
    // Host "b" declares no override, so it falls back to the project's default module set.
    seed_effective_cache("rev-b", &[disko_b.clone()], "disko.b.enable");

    let root = std::env::temp_dir().join(format!(
        "knixl-proj-oracle-module-effective-set-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("hosts")).unwrap();
    fs::write(
        root.join("hosts/a.kdl"),
        "host \"a\" {\n    system \"x86_64-linux\"\n    nixpkgs release=\"24.11\"\n    oracle-modules {\n        module \"disko\" flake=\"github:nix-community/disko\"\n    }\n}\n",
    )
    .unwrap();
    fs::write(
        root.join("hosts/b.kdl"),
        "host \"b\" {\n    system \"x86_64-linux\"\n}\n",
    )
    .unwrap();

    let mut baselines = BTreeMap::new();
    baselines.insert(
        "a".to_string(),
        HostBaseline {
            release: "24.11".into(),
            nixpkgs_rev: "rev-a".into(),
            options_hash: String::new(),
            modules: vec![OracleModulePin {
                name: "disko".into(),
                url: disko_a.0.clone(),
                rev: disko_a.1.clone(),
                attr: disko_a.2.clone(),
            }],
        },
    );
    let lock = Lock {
        version: 1,
        tool: "0.3.1".parse().unwrap(),
        formatter: FormatterPin {
            name: "identity".into(),
            version: "0".into(),
        },
        oracle: OraclePin {
            nixpkgs_rev: "rev-b".into(),
            options_hash: String::new(),
            modules: vec![OracleModulePin {
                name: "disko".into(),
                url: disko_b.0.clone(),
                rev: disko_b.1.clone(),
                attr: disko_b.2.clone(),
            }],
        },
        module_sources: Vec::new(),
        inputs: BTreeMap::new(),
        modules: BTreeMap::new(),
        outputs: Vec::new(),
        pins: BTreeMap::new(),
        baselines,
    };
    fs::write(root.join("knixl.lock.kdl"), lock.render()).unwrap();

    let project = gather(&root, &identity_formatter(), "0.3.1".parse().unwrap()).expect("gather");

    assert!(!project
        .inputs
        .validation_errors
        .contains(&"host \"a\": oracle-modules requires a declared nixpkgs release".to_string()));

    let disko_a_enable = ident_path(&["disko", "a", "enable"]);
    let disko_b_enable = ident_path(&["disko", "b", "enable"]);

    // Host a validated against its OWN effective set: knows disko.a.enable, not disko.b.enable,
    // and not the base-only rev-a cache's system.stateVersion (proving the augmented cache, not
    // the bare rev cache, is what got loaded).
    assert!(project.oracles["a"]
        .check(&disko_a_enable, &NixExpr::Bool(true))
        .is_ok());
    assert!(project.oracles["a"]
        .check(&disko_b_enable, &NixExpr::Bool(true))
        .is_err());

    // Host b declared no override: falls back to the project's default module set.
    assert!(project.oracles["b"]
        .check(&disko_b_enable, &NixExpr::Bool(true))
        .is_ok());
    assert!(project.oracles["b"]
        .check(&disko_a_enable, &NixExpr::Bool(true))
        .is_err());

    std::env::remove_var("XDG_CACHE_HOME");
    let _ = fs::remove_dir_all(&tmp);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn host_oracle_modules_without_a_declared_release_is_a_validation_error() {
    let _guard = ENV_LOCK.lock().unwrap();
    let tmp = std::env::temp_dir().join(format!(
        "knixl-oracle-module-no-release-cache-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&tmp);
    std::env::set_var("XDG_CACHE_HOME", &tmp);

    let root = std::env::temp_dir().join(format!(
        "knixl-proj-oracle-module-no-release-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("hosts")).unwrap();
    fs::write(
        root.join("hosts/a.kdl"),
        "host \"a\" {\n    system \"x86_64-linux\"\n    oracle-modules {\n        module \"disko\" flake=\"github:nix-community/disko\"\n    }\n}\n",
    )
    .unwrap();

    let project = gather(&root, &identity_formatter(), "0.3.1".parse().unwrap()).expect("gather");
    assert!(
        project
            .inputs
            .validation_errors
            .iter()
            .any(|e| e.contains("a")
                && e.contains("oracle-modules")
                && e.contains("nixpkgs release")),
        "got: {:?}",
        project.inputs.validation_errors
    );

    std::env::remove_var("XDG_CACHE_HOME");
    let _ = fs::remove_dir_all(&tmp);
    let _ = fs::remove_dir_all(&root);
}
