//! End-to-end tests driving the real `knixl` binary against a temp project. An identity
//! formatter (`cat`, via KNIXL_FORMATTER) stands in for nixfmt so these run without it.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn examples() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples")
}

fn temp_project(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("knixl-cli-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("hosts")).unwrap();
    fs::create_dir_all(root.join("modules/web-service")).unwrap();
    let ex = examples();
    for host in ["web.kdl", "db.kdl"] {
        fs::copy(ex.join("hosts").join(host), root.join("hosts").join(host)).unwrap();
    }
    fs::copy(ex.join("knixl.lock.kdl"), root.join("knixl.lock.kdl")).unwrap();
    fs::copy(
        ex.join("../modules/web-service/knixl-module.kdl"),
        root.join("modules/web-service/knixl-module.kdl"),
    )
    .unwrap();
    root
}

fn knixl(root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_knixl"))
        .args(args)
        .current_dir(root)
        .env("KNIXL_FORMATTER", "cat")
        .output()
        .expect("run knixl")
}

#[test]
fn doc_prints_a_typed_reference() {
    let root = temp_project("doc");
    let out = knixl(&root, &["doc", "host"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success());
    assert!(stdout.contains("host:"), "got: {stdout}");
    assert!(stdout.contains("system : string (required)"), "got: {stdout}");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn check_exits_regen_pending_when_nothing_is_generated() {
    let root = temp_project("check");
    let out = knixl(&root, &["check"]);
    // docs/05: Stale/Missing/Orphaned => exit 6 (RegenPending).
    assert_eq!(out.status.code(), Some(6), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("missing"), "got: {stdout}");
    assert!(stdout.contains("generated/hosts/web.nix"), "got: {stdout}");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn plan_defaults_to_exit_zero() {
    let root = temp_project("plan");
    let out = knixl(&root, &["plan"]);
    assert_eq!(out.status.code(), Some(0));
    let _ = fs::remove_dir_all(&root);
}
