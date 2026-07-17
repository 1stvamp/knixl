//! A host's declared `nixpkgs release=".."` (issue #22) that has no matching lock baseline
//! is a validation error naming the fix (`knixl upgrade`), surfaced the same way an
//! unresolved pin is: on `Inputs.validation_errors`, which `Plan::compute` carries through
//! to `plan.has_validation_errors()`.

use std::fs;
use std::path::PathBuf;

use knixl_lock::reconcile::Plan;
use knixl_nix::Formatter;
use knixl_pipeline::gather::gather;

fn identity_formatter() -> Formatter {
    Formatter { name: "identity".into(), version: "0".into(), bin: PathBuf::from("cat") }
}

fn temp_root(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("knixl-baseline-validation-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("hosts")).unwrap();
    root
}

#[test]
fn declared_release_with_no_lock_baseline_is_a_validation_error() {
    let root = temp_root("no-baseline");
    fs::write(
        root.join("hosts/web.kdl"),
        "host \"web\" {\n    system \"x86_64-linux\"\n    nixpkgs release=\"25.05\"\n}\n",
    )
    .unwrap();

    let project = gather(&root, &identity_formatter(), "0.3.1".parse().unwrap()).expect("gather");
    let plan = Plan::compute(&project.inputs, &project.disk, &project.lock, &project.versions);

    assert!(plan.has_validation_errors(), "unresolved baseline must surface as a validation error");
    assert!(
        plan.validation_errors.iter().any(|e| e.contains("web") && e.contains("25.05") && e.contains("knixl upgrade")),
        "got: {:?}",
        plan.validation_errors
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn declared_release_matching_the_lock_baseline_has_no_validation_error() {
    let root = temp_root("matching-baseline");
    fs::write(
        root.join("hosts/web.kdl"),
        "host \"web\" {\n    system \"x86_64-linux\"\n    nixpkgs release=\"25.05\"\n}\n",
    )
    .unwrap();

    // Seed a lock whose "web" baseline already matches the declared release.
    let formatter = identity_formatter();
    let tool: semver::Version = "0.3.1".parse().unwrap();
    let seed = gather(&root, &formatter, tool.clone()).expect("gather (seed)");
    let mut lock = seed.lock;
    lock.baselines.insert(
        "web".to_string(),
        knixl_lock::model::HostBaseline {
            release: "25.05".into(),
            nixpkgs_rev: "abc123".into(),
            options_hash: String::new(),
        },
    );
    fs::write(root.join("knixl.lock.kdl"), lock.render()).unwrap();

    let project = gather(&root, &formatter, tool).expect("gather");
    assert!(
        !project.inputs.validation_errors.iter().any(|e| e.contains("is not resolved")),
        "got: {:?}",
        project.inputs.validation_errors
    );

    let _ = fs::remove_dir_all(&root);
}
