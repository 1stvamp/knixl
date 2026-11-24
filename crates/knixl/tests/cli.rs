//! End-to-end tests driving the real `knixl` binary against a temp project. An identity
//! formatter (`cat`, via KNIXL_FORMATTER) stands in for nixfmt so these run without it.

mod common;

use std::fs;
use std::process::Command;

use common::*;

/// #85: a typo used to be a warning, so `check` exited 0 with the option missing from the
/// generated Nix. These pin the exit code and the machine-readable output at the CLI boundary,
/// which is where the issue was actually felt.
#[test]
fn a_typo_in_a_host_refuses_with_the_validation_exit_code() {
    let root = temp_project("typo-exit");
    fs::write(
        root.join("hosts/typo.kdl"),
        "host \"typo\" {\n    system \"x86_64-linux\"\n    os {\n        timezon \"Europe/London\"\n    }\n}\n",
    )
    .unwrap();

    let gen = knixl(&root, &["generate"]);
    assert_eq!(
        gen.status.code(),
        Some(5),
        "generate must refuse: {}",
        String::from_utf8_lossy(&gen.stderr)
    );
    assert!(
        !root.join("generated/hosts/typo.nix").exists(),
        "nothing should be written from KDL knixl cannot fully interpret"
    );

    let check = knixl(&root, &["check"]);
    assert_eq!(check.status.code(), Some(5), "check must refuse too");
    let stderr = String::from_utf8_lossy(&check.stderr);
    assert!(
        stderr.contains("timezon") && stderr.contains("hosts/typo.kdl"),
        "the error should name the child and locate the file: {stderr}"
    );

    let _ = fs::remove_dir_all(&root);
}

/// #131: a mistake in knixl.kdl is the user's input, so it is a Validation (5), never the
/// Internal (1) that tells them to report a bug.
#[test]
fn a_mistake_in_knixl_kdl_refuses_with_the_validation_exit_code() {
    let root = temp_project("project-config-exit");
    fs::write(
        root.join("knixl.kdl"),
        "system {\n    state-version \"25.11\"\n    input \"nixpkgs\" url=\"github:NixOS/nixpkgs/nixos-25.11\"\n}\n",
    )
    .unwrap();
    for cmd in ["plan", "check", "generate"] {
        let out = knixl(&root, &[cmd]);
        assert_eq!(
            out.status.code(),
            Some(5),
            "{cmd}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(String::from_utf8_lossy(&out.stderr).contains("carries a ref"));
    }
    let _ = fs::remove_dir_all(&root);
}

/// #130: validation errors carry the line, and an oracle refusal reads as a sentence naming
/// the file and the option rather than a Rust Debug struct.
#[test]
fn validation_errors_name_the_line_and_the_option() {
    let root = temp_project("readable-validation");
    fs::write(
        root.join("hosts/typo.kdl"),
        "host \"typo\" {\n    system \"x86_64-linux\"\n\n    opensh {\n    }\n}\n",
    )
    .unwrap();
    let out = knixl(&root, &["plan"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("hosts/typo.kdl:4: no module claims node `opensh`"),
        "{stderr}"
    );

    fs::remove_file(root.join("hosts/typo.kdl")).unwrap();
    fs::create_dir_all(root.join("modules/bogus")).unwrap();
    fs::write(
        root.join("modules/bogus/knixl-module.kdl"),
        "module name=\"bogus\" version=\"0.1.0\" {\n    claims-node \"bogus\"\n    schema {\n    }\n    emit {\n        set \"services.nopenope.enable\" #true\n    }\n}\n",
    )
    .unwrap();
    fs::write(
        root.join("hosts/bogus.kdl"),
        "host \"bogus\" {\n    system \"x86_64-linux\"\n    bogus\n}\n",
    )
    .unwrap();
    let options = root.join("options.json");
    fs::write(&options, "{}").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_knixl"))
        .arg("plan")
        .current_dir(&root)
        .env("KNIXL_FORMATTER", "cat")
        .env("KNIXL_NIX_BUILD", "false")
        .env("KNIXL_OPTIONS_JSON", &options)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(5));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(
            "hosts/bogus.kdl: `services.nopenope.enable` is not a NixOS option in this host's nixpkgs"
        ),
        "{stderr}"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn json_check_carries_validation_errors() {
    let root = temp_project("json-validation");
    fs::write(
        root.join("hosts/typo.kdl"),
        "host \"typo\" {\n    system \"x86_64-linux\"\n    mystery-service\n}\n",
    )
    .unwrap();

    let out = knixl(&root, &["--json", "check"]);
    assert_eq!(out.status.code(), Some(5));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"validation\"") && stdout.contains("mystery-service"),
        "--json must carry the error, not just stderr: {stdout}"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn json_plan_carries_warnings_alongside_files() {
    let root = temp_project("json-warnings");
    let out = knixl(&root, &["--json", "plan"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"files\"") && stdout.contains("\"warnings\""),
        "the plan object should carry both keys: {stdout}"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn doc_prints_a_typed_reference() {
    let root = temp_project("doc");
    let out = knixl(&root, &["doc", "host"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success());
    assert!(stdout.contains("host:"), "got: {stdout}");
    assert!(
        stdout.contains("system : string (required)"),
        "got: {stdout}"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn check_exits_regen_pending_when_nothing_is_generated() {
    let root = temp_project("check");
    let out = knixl(&root, &["check"]);
    // docs/05: Stale/Missing/Orphaned => exit 6 (RegenPending).
    assert_eq!(
        out.status.code(),
        Some(6),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
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

#[test]
fn drift_is_detected_and_refused_without_accept_drift() {
    let root = temp_project("drift");
    assert_eq!(knixl(&root, &["generate"]).status.code(), Some(0));

    // Hand-edit a generated file: disk now diverges from the lock (tainted).
    let web = root.join("generated/hosts/web.nix");
    let edited = format!("{}\n# hand edit\n", fs::read_to_string(&web).unwrap());
    fs::write(&web, edited).unwrap();

    // check => Drift (exit 3).
    assert_eq!(knixl(&root, &["check"]).status.code(), Some(3));

    // generate refuses to clobber the edit (exit 3), leaving it in place.
    assert_eq!(knixl(&root, &["generate"]).status.code(), Some(3));
    assert!(fs::read_to_string(&web).unwrap().contains("# hand edit"));

    // --accept-drift knowingly overwrites (exit 0), discarding the edit.
    assert_eq!(
        knixl(&root, &["generate", "--accept-drift"]).status.code(),
        Some(0)
    );
    assert!(!fs::read_to_string(&web).unwrap().contains("# hand edit"));

    let _ = fs::remove_dir_all(&root);
}

/// #135: drift outranks a pending regeneration, so a hand edit beside an orphan still exits 3.
#[test]
fn generate_exits_drift_when_drift_and_an_orphan_coincide() {
    let root = temp_project("drift-and-orphan");
    assert_eq!(knixl(&root, &["generate"]).status.code(), Some(0));
    let web = root.join("generated/hosts/web.nix");
    fs::write(
        &web,
        format!("{}\n# hand edit\n", fs::read_to_string(&web).unwrap()),
    )
    .unwrap();
    fs::remove_file(root.join("hosts/db.kdl")).unwrap();
    let out = knixl(&root, &["generate"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("drift:") && stderr.contains("orphan:"),
        "{stderr}"
    );
    assert_eq!(out.status.code(), Some(3), "{stderr}");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn prune_deletes_orphaned_files() {
    let root = temp_project("prune");
    assert_eq!(knixl(&root, &["generate"]).status.code(), Some(0));

    // An orphan: a knixl-headed file no host produces anymore.
    let orphan = root.join("generated/hosts/old.nix");
    fs::write(
        &orphan,
        "# Generated by knixl 0.3.1 from hosts/old.kdl\n{ }\n",
    )
    .unwrap();

    assert_eq!(
        knixl(&root, &["check"]).status.code(),
        Some(6),
        "orphan => RegenPending"
    );
    assert_eq!(
        knixl(&root, &["generate", "--prune"]).status.code(),
        Some(0)
    );
    assert!(!orphan.exists(), "--prune removed the orphan");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn check_reports_a_validation_error_for_an_unresolved_declared_release() {
    let root = temp_project("check-baseline-unresolved");
    declare_baseline_release(&root, "25.05");

    let out = knixl(&root, &["check"]);
    assert_eq!(
        out.status.code(),
        Some(5),
        "stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("web") && stderr.contains("25.05") && stderr.contains("knixl upgrade"),
        "stderr: {stderr}"
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn generate_writes_files_then_check_is_clean() {
    let root = temp_project("gen");

    let gen = knixl(&root, &["generate"]);
    assert_eq!(
        gen.status.code(),
        Some(0),
        "generate: {}",
        String::from_utf8_lossy(&gen.stderr)
    );

    for f in ["web.nix", "db.nix", "db-backup.nix"] {
        assert!(
            root.join("generated/hosts").join(f).exists(),
            "{f} was written"
        );
    }

    // After a clean apply the lock matches disk, so check is Clean (exit 0).
    let check = knixl(&root, &["check"]);
    assert_eq!(
        check.status.code(),
        Some(0),
        "check after generate: {}",
        String::from_utf8_lossy(&check.stdout)
    );
    let _ = fs::remove_dir_all(&root);
}

/// #35 phase 3: `plan` stays offline. It must never invoke the module resolver (the shim
/// below fails loudly if it is ever run) nor write a module pin to the lock, even though the
/// project declares one.
#[test]
fn plan_does_not_resolve_or_write_a_declared_oracle_module() {
    let root = temp_project("plan-oracle-module");
    declare_project_oracle_module(&root, "disko", "github:nix-community/disko");

    let never_called = resolver_shim("plan-oracle-module-never", "", "should not be called", 1);

    let out = Command::new(env!("CARGO_BIN_EXE_knixl"))
        .args(["plan"])
        .current_dir(&root)
        .env("KNIXL_FORMATTER", "cat")
        .env("KNIXL_NIX_BUILD", "false")
        .env("KNIXL_MODULE_RESOLVER", &never_called)
        .output()
        .expect("run knixl plan");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let lock_path = root.join("knixl.lock.kdl");
    if lock_path.exists() {
        let lock = fs::read_to_string(&lock_path).unwrap();
        assert!(
            !lock.contains("oracle-module"),
            "plan must not resolve or write module pins: {lock}"
        );
    }

    let _ = fs::remove_file(&never_called);
    let _ = fs::remove_dir_all(&root);
}

/// As above, but via `check`: the same refusal must apply to every command that reconciles a
/// project (gather's own validation), not just `install`'s pre-pass.
#[test]
fn check_refuses_a_host_oracle_modules_override_without_a_declared_release() {
    let root = temp_project("check-host-oracle-module-no-release");
    declare_host_oracle_module(&root, "web", "disko", "github:nix-community/disko");

    let out = knixl(&root, &["check"]);
    assert_eq!(
        out.status.code(),
        Some(5),
        "stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("web")
            && stderr.contains("oracle-modules")
            && stderr.contains("nixpkgs release"),
        "stderr: {stderr}"
    );

    let _ = fs::remove_dir_all(&root);
}

/// #108: `doc` only needs the module registry, so an invalid or unparsable host elsewhere in
/// the project must not stop it printing a reference.
#[test]
fn doc_works_whatever_state_the_hosts_are_in() {
    let root = temp_project("doc-invalid-hosts");
    fs::write(
        root.join("hosts/typo.kdl"),
        "host \"typo\" {\n    os {\n        timezon \"UTC\"\n    }\n}\n",
    )
    .unwrap();
    fs::write(root.join("hosts/broken.kdl"), "host \"broken\" {\n").unwrap();

    let out = knixl(&root, &["doc", "openssh"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("openssh"));

    let _ = fs::remove_dir_all(&root);
}
