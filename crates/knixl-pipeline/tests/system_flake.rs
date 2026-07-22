//! Wiring the system-assembly flake (ADR 0009) into `gather`: a project with a `system {}`
//! block in `knixl.kdl` gets its flake rendered, formatted, and inserted into both the
//! generated-files map and the lock's expected outputs, exactly like any other generated
//! file. A host with no resolved baseline blocks the whole flake (a partial flake would lie
//! about the fleet); a project without a `system {}` block emits none of this at all.

use std::fs;
use std::path::PathBuf;

use knixl_lock::model::HostBaseline;
use knixl_lock::reconcile::Plan;
use knixl_nix::Formatter;
use knixl_pipeline::gather::gather;

fn identity_formatter() -> Formatter {
    Formatter {
        name: "identity".into(),
        version: "0".into(),
        bin: PathBuf::from("cat"),
    }
}

fn temp_root(tag: &str) -> PathBuf {
    let root =
        std::env::temp_dir().join(format!("knixl-system-flake-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("hosts")).unwrap();
    root
}

#[test]
fn system_block_emits_flake_pinned_to_every_host_baseline() {
    let root = temp_root("emits");
    fs::write(
        root.join("knixl.kdl"),
        "system {\n    state-version \"25.05\"\n}\n",
    )
    .unwrap();
    fs::write(
        root.join("hosts/web.kdl"),
        "host \"web\" {\n    system \"x86_64-linux\"\n    nixpkgs release=\"25.05\"\n}\n",
    )
    .unwrap();

    let formatter = identity_formatter();
    let tool: semver::Version = "0.3.1".parse().unwrap();

    // Seed a lock with a resolved baseline for "web" (mirrors baseline_validation.rs).
    let seed = gather(&root, &formatter, tool.clone()).expect("gather (seed)");
    let mut lock = seed.lock;
    lock.baselines.insert(
        "web".to_string(),
        HostBaseline {
            release: "25.05".into(),
            nixpkgs_rev: "abcdef1234567890".into(),
            options_hash: String::new(),
            modules: Vec::new(),
        },
    );
    fs::write(root.join("knixl.lock.kdl"), lock.render()).unwrap();

    let project = gather(&root, &formatter, tool).expect("gather");

    let flake_path = PathBuf::from("generated/flake.nix");
    let flake = project
        .generated
        .get(&flake_path)
        .expect("generated/flake.nix present in project.generated");
    assert!(flake.contains("nixosConfigurations"), "got: {flake}");
    assert!(flake.contains("\"web\""), "got: {flake}");
    assert!(flake.contains("abcdef1234567890"), "got: {flake}");

    let plan = Plan::compute(
        &project.inputs,
        &project.disk,
        &project.lock,
        &project.versions,
    );
    assert!(
        plan.lock_next.outputs.iter().any(|o| o.path == flake_path),
        "lock outputs missing generated/flake.nix: {:?}",
        plan.lock_next
            .outputs
            .iter()
            .map(|o| &o.path)
            .collect::<Vec<_>>()
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn system_block_with_an_unresolved_host_baseline_is_a_validation_error() {
    let root = temp_root("missing-baseline");
    fs::write(
        root.join("knixl.kdl"),
        "system {\n    state-version \"25.05\"\n}\n",
    )
    .unwrap();
    // "web" declares no nixpkgs release at all, so it never gets a lock baseline: system
    // {} still requires one to pin nixpkgs for it.
    fs::write(
        root.join("hosts/web.kdl"),
        "host \"web\" {\n    system \"x86_64-linux\"\n}\n",
    )
    .unwrap();

    let project = gather(&root, &identity_formatter(), "0.3.1".parse().unwrap()).expect("gather");

    assert!(
        project
            .inputs
            .validation_errors
            .iter()
            .any(|e| e.contains("web") && e.contains("system")),
        "got: {:?}",
        project.inputs.validation_errors
    );
    assert!(
        !project
            .generated
            .contains_key(&PathBuf::from("generated/flake.nix")),
        "a partial flake must not be emitted when a host is missing a resolved baseline"
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn no_system_block_emits_no_flake() {
    let root = temp_root("no-system");
    fs::write(
        root.join("hosts/web.kdl"),
        "host \"web\" {\n    system \"x86_64-linux\"\n}\n",
    )
    .unwrap();

    let project = gather(&root, &identity_formatter(), "0.3.1".parse().unwrap()).expect("gather");

    assert!(
        !project
            .generated
            .contains_key(&PathBuf::from("generated/flake.nix")),
        "no system {{}} block declared, so no flake should be emitted"
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn knixl_kdl_without_a_system_block_is_unchanged() {
    // A knixl.kdl on disk that declares no system {} block must not change generate: gather
    // succeeds and emits no flake, the same as a project with no knixl.kdl at all.
    let root = temp_root("config-no-system");
    fs::write(root.join("knixl.kdl"), "nixpkgs release=\"25.05\"\n").unwrap();
    fs::write(
        root.join("hosts/web.kdl"),
        "host \"web\" {\n    system \"x86_64-linux\"\n}\n",
    )
    .unwrap();

    let project = gather(&root, &identity_formatter(), "0.3.1".parse().unwrap()).expect("gather");

    assert!(
        !project
            .generated
            .contains_key(&PathBuf::from("generated/flake.nix")),
        "a knixl.kdl without a system {{}} block should emit no flake"
    );

    let _ = fs::remove_dir_all(&root);
}
