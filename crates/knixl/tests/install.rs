//! `knixl install` end to end: hosts, pins, strategies, baselines and oracle modules.

mod common;

use std::fs;
use std::process::Command;

use common::*;

#[test]
fn install_build_refuses_when_the_build_fails() {
    let root = temp_project("install-build-fail");
    let eval_shim = nix_shim("build-fail-eval", true); // package resolves + parses
    let bad_build = build_shim("build-fail", false); // build exits non-zero
    let out = Command::new(env!("CARGO_BIN_EXE_knixl"))
        .args(["install", "ripgrep", "--host", "web", "--yes", "--build"])
        .current_dir(&root)
        .env("KNIXL_FORMATTER", "cat")
        .env("KNIXL_NIX_BUILD", "false")
        .env("KNIXL_NIX", &eval_shim)
        .env("KNIXL_NIX_BUILD", &bad_build)
        .output()
        .expect("run knixl install");
    assert_eq!(
        out.status.code(),
        Some(5),
        "build failure exits Validation (5): {out:?}"
    );
    // The KDL is untouched (reverted / never written): the build gate runs before commit.
    let kdl = fs::read_to_string(root.join("hosts/web.kdl")).unwrap();
    assert!(!kdl.contains("ripgrep"), "no draft left behind: {kdl}");
    let _ = fs::remove_file(&eval_shim);
    let _ = fs::remove_file(&bad_build);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn install_build_allows_unfree_only_when_the_host_config_does() {
    let unfree = "config.allowUnfree = true;";
    let allowed = install_build_args("unfree-yes", true);
    assert!(allowed.contains(unfree), "build args: {allowed}");
    let denied = install_build_args("unfree-no", false);
    assert!(!denied.is_empty(), "the build ran");
    assert!(!denied.contains(unfree), "build args: {denied}");
}

#[test]
fn install_drafts_verifies_and_regenerates() {
    let root = temp_project("install");
    let shim = nix_shim("ok", true);
    let out = Command::new(env!("CARGO_BIN_EXE_knixl"))
        .args(["install", "ripgrep", "--host", "web", "--yes"])
        .current_dir(&root)
        .env("KNIXL_FORMATTER", "cat")
        .env("KNIXL_NIX_BUILD", "false")
        .env("KNIXL_NIX", &shim)
        .output()
        .expect("run knixl install");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let kdl = fs::read_to_string(root.join("hosts/web.kdl")).unwrap();
    assert!(kdl.contains("package \"ripgrep\""), "kdl edited: {kdl}");
    assert!(
        kdl.contains("web-service \"example.com\""),
        "existing content kept: {kdl}"
    );

    let nix = fs::read_to_string(root.join("generated/hosts/web.nix")).unwrap();
    assert!(
        nix.contains("environment.systemPackages"),
        "systemPackages emitted: {nix}"
    );
    assert!(
        nix.contains("pkgs.ripgrep"),
        "package reference emitted: {nix}"
    );

    let check = Command::new(env!("CARGO_BIN_EXE_knixl"))
        .args(["check"])
        .current_dir(&root)
        .env("KNIXL_FORMATTER", "cat")
        .env("KNIXL_NIX_BUILD", "false")
        .output()
        .expect("run check");
    assert_eq!(
        check.status.code(),
        Some(0),
        "check after install: {}",
        String::from_utf8_lossy(&check.stdout)
    );
    let _ = fs::remove_file(&shim);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn install_refuses_a_nonexistent_package() {
    let root = temp_project("install-bad");
    let shim = nix_shim("bad", false); // package does not resolve
    let out = Command::new(env!("CARGO_BIN_EXE_knixl"))
        .args(["install", "nnoopkg", "--host", "web", "--yes"])
        .current_dir(&root)
        .env("KNIXL_FORMATTER", "cat")
        .env("KNIXL_NIX_BUILD", "false")
        .env("KNIXL_NIX", &shim)
        .output()
        .expect("run knixl install");
    assert_eq!(out.status.code(), Some(5), "validation exit");
    // The KDL is untouched (reverted / never written).
    let kdl = fs::read_to_string(root.join("hosts/web.kdl")).unwrap();
    assert!(!kdl.contains("nnoopkg"), "no draft left behind: {kdl}");
    let _ = fs::remove_file(&shim);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn install_refuses_a_package_the_host_takes_from_an_input() {
    let root = temp_project("install-from");
    let host = root.join("hosts/web.kdl");
    let src = fs::read_to_string(&host).unwrap();
    let close = src.rfind('}').unwrap();
    let with_from = format!(
        "{}    package \"ripgrep\" from=\"nixpkgs-unstable\"\n{}",
        &src[..close],
        &src[close..]
    );
    fs::write(&host, &with_from).unwrap();
    let shim = nix_shim("from", true);
    let out = Command::new(env!("CARGO_BIN_EXE_knixl"))
        .args(["install", "ripgrep", "--host", "web", "--yes"])
        .current_dir(&root)
        .env("KNIXL_FORMATTER", "cat")
        .env("KNIXL_NIX_BUILD", "false")
        .env("KNIXL_NIX", &shim)
        .output()
        .expect("run knixl install");
    assert_eq!(
        out.status.code(),
        Some(5),
        "refusal exits Validation (5): {out:?}"
    );
    assert_eq!(
        fs::read_to_string(&host).unwrap(),
        with_from,
        "node left alone"
    );
    let _ = fs::remove_file(&shim);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn install_strict_errors_when_nix_absent() {
    let root = temp_project("install-strict");
    let out = Command::new(env!("CARGO_BIN_EXE_knixl"))
        .args(["install", "ripgrep", "--host", "web", "--yes", "--strict"])
        .current_dir(&root)
        .env("KNIXL_FORMATTER", "cat")
        .env("KNIXL_NIX_BUILD", "false")
        .env("KNIXL_NIX", "/nonexistent/knixl-no-nix")
        .output()
        .expect("run knixl install");
    assert_eq!(
        out.status.code(),
        Some(5),
        "strict + no nix => validation exit"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn install_pkg_at_version_refuses_when_resolver_cannot_find_it() {
    let root = temp_project("install-version-nf");
    let ok_eval = nix_shim("version-nf-eval", true); // package resolves + parses
    let nf_resolver = resolver_shim("nf", "", "not found", 1);
    let out = Command::new(env!("CARGO_BIN_EXE_knixl"))
        .args(["install", "htop@9.9.9", "--host", "web", "--yes"])
        .current_dir(&root)
        .env("KNIXL_FORMATTER", "cat")
        .env("KNIXL_NIX_BUILD", "false")
        .env("KNIXL_NIX", &ok_eval)
        .env("KNIXL_PIN_RESOLVER", &nf_resolver)
        .output()
        .expect("run knixl install");
    assert_eq!(
        out.status.code(),
        Some(5),
        "unresolvable version refuses (Validation): {out:?}"
    );
    // The KDL is untouched (reverted / never written): the pin gate runs before commit.
    let kdl = fs::read_to_string(root.join("hosts/web.kdl")).unwrap();
    assert!(!kdl.contains("htop"), "no draft left behind: {kdl}");
    let _ = fs::remove_file(&ok_eval);
    let _ = fs::remove_file(&nf_resolver);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn install_pkg_at_version_resolves_writes_the_pin_and_regenerates() {
    let root = temp_project("install-version-ok");
    let ok_eval = nix_shim("version-ok-eval", true); // package resolves + parses
    let resolver = resolver_shim("ok", "abc123", "", 0);
    // Shim the build oracle to succeed so strategy selection deterministically picks
    // override (tried first) without a real network build.
    let ok_build = build_shim("version-ok-build", true);
    let out = Command::new(env!("CARGO_BIN_EXE_knixl"))
        .args(["install", "htop@3.2.1", "--host", "web", "--yes"])
        .current_dir(&root)
        .env("KNIXL_FORMATTER", "cat")
        .env("KNIXL_NIX_BUILD", "false")
        .env("KNIXL_NIX", &ok_eval)
        .env("KNIXL_NIX_BUILD", &ok_build)
        .env("KNIXL_PIN_RESOLVER", &resolver)
        .output()
        .expect("run knixl install");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let kdl = fs::read_to_string(root.join("hosts/web.kdl")).unwrap();
    assert!(
        kdl.contains("package \"htop\" version=\"3.2.1\""),
        "kdl edited: {kdl}"
    );

    let lock = fs::read_to_string(root.join("knixl.lock.kdl")).unwrap();
    assert!(
        lock.contains("pin \"htop\" version=\"3.2.1\""),
        "pin recorded: {lock}"
    );
    assert!(
        lock.contains("nixpkgs-rev=\"abc123\""),
        "rev recorded: {lock}"
    );

    let nix = fs::read_to_string(root.join("generated/hosts/web.nix")).unwrap();
    assert!(
        nix.contains("fetchGit"),
        "pinned import uses fetchGit: {nix}"
    );
    assert!(
        nix.contains("abc123"),
        "pinned import carries the commit: {nix}"
    );

    let _ = fs::remove_file(&ok_eval);
    let _ = fs::remove_file(&resolver);
    let _ = fs::remove_file(&ok_build);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn install_pkg_at_version_reverts_the_pin_when_commit_fails_after_write_pin() {
    let root = temp_project("install-version-revert");
    let parse_fail_eval = nix_shim_parse_fails("revert");
    let resolver = resolver_shim("revert-ok", "abc123", "", 0);
    // As above: shim the build oracle so override is picked deterministically.
    let ok_build = build_shim("version-revert-build", true);
    let out = Command::new(env!("CARGO_BIN_EXE_knixl"))
        .args(["install", "htop@3.2.1", "--host", "web", "--yes"])
        .current_dir(&root)
        .env("KNIXL_FORMATTER", "cat")
        .env("KNIXL_NIX_BUILD", "false")
        .env("KNIXL_NIX", &parse_fail_eval)
        .env("KNIXL_NIX_BUILD", &ok_build)
        .env("KNIXL_PIN_RESOLVER", &resolver)
        .output()
        .expect("run knixl install");
    assert_eq!(
        out.status.code(),
        Some(5),
        "parse failure after the pin gate: {out:?}"
    );

    // The KDL is reverted (no dangling `version` prop)...
    let kdl = fs::read_to_string(root.join("hosts/web.kdl")).unwrap();
    assert!(!kdl.contains("htop"), "kdl reverted: {kdl}");

    // ...and the pin `write_pin` recorded before the parse check must not be left dangling.
    let lock_path = root.join("knixl.lock.kdl");
    if lock_path.exists() {
        let lock = fs::read_to_string(&lock_path).unwrap();
        assert!(
            !lock.contains("pin \"htop\""),
            "pin not left dangling after revert: {lock}"
        );
    }

    let _ = fs::remove_file(&parse_fail_eval);
    let _ = fs::remove_file(&resolver);
    let _ = fs::remove_file(&ok_build);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn install_pkg_at_version_prefers_override_when_it_builds() {
    let root = temp_project("install-strategy-override");
    let ok_eval = nix_shim("strategy-override-eval", true); // package resolves + parses
    let resolver = resolver_shim("strategy-override", "abc123", "", 0);
    let build = strategy_build_shim("override");
    let out = Command::new(env!("CARGO_BIN_EXE_knixl"))
        .args(["install", "htop@3.2.1", "--host", "web", "--yes"])
        .current_dir(&root)
        .env("KNIXL_FORMATTER", "cat")
        .env("KNIXL_NIX_BUILD", "false")
        .env("KNIXL_NIX", &ok_eval)
        .env("KNIXL_NIX_BUILD", &build)
        .env("KNIXL_PIN_RESOLVER", &resolver)
        .output()
        .expect("run knixl install");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let lock = fs::read_to_string(root.join("knixl.lock.kdl")).unwrap();
    assert!(
        lock.contains("strategy=\"override\""),
        "override strategy recorded: {lock}"
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("via override"),
        "status line names the strategy: {stdout}"
    );

    let _ = fs::remove_file(&ok_eval);
    let _ = fs::remove_file(&resolver);
    let _ = fs::remove_file(&build);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn install_pkg_at_version_no_abi_check_skips_the_build_shim() {
    let root = temp_project("install-no-abi-check");
    let ok_eval = nix_shim("no-abi-check-eval", true); // package resolves + parses
    let resolver = resolver_shim("no-abi-check", "abc123", "", 0);
    // Always-failing shim: proves --no-abi-check truly skips the build (otherwise both
    // candidates fail and exit 5).
    let never_build = build_shim("no-abi-check-build", false);
    let out = Command::new(env!("CARGO_BIN_EXE_knixl"))
        .args([
            "install",
            "htop@3.2.1",
            "--host",
            "web",
            "--yes",
            "--no-abi-check",
        ])
        .current_dir(&root)
        .env("KNIXL_FORMATTER", "cat")
        .env("KNIXL_NIX_BUILD", "false")
        .env("KNIXL_NIX", &ok_eval)
        .env("KNIXL_NIX_BUILD", &never_build)
        .env("KNIXL_PIN_RESOLVER", &resolver)
        .output()
        .expect("run knixl install");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let lock = fs::read_to_string(root.join("knixl.lock.kdl")).unwrap();
    assert!(lock.contains("pin \"htop\""), "pin recorded: {lock}");
    assert!(
        !lock.contains("strategy=\"override\""),
        "commit-mix recorded (no strategy attr): {lock}"
    );

    let _ = fs::remove_file(&ok_eval);
    let _ = fs::remove_file(&resolver);
    let _ = fs::remove_file(&never_build);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn install_with_a_declared_release_resolves_and_writes_the_baseline() {
    let root = temp_project("install-baseline");
    declare_baseline_release(&root, "25.05");

    let ok_eval = nix_shim("baseline-eval", true); // package resolves + parses
    let pin_resolver = resolver_shim("baseline-pin", "abc123", "", 0);
    let baseline_resolver = resolver_shim("baseline-rev", "deadbeef", "", 0);
    // Distinct pin/baseline revs would otherwise build-test for real; shim it to succeed.
    let ok_build = build_shim("baseline-build", true);

    let out = Command::new(env!("CARGO_BIN_EXE_knixl"))
        .args(["install", "htop@3.2.1", "--host", "web", "--yes"])
        .current_dir(&root)
        .env("KNIXL_FORMATTER", "cat")
        .env("KNIXL_NIX_BUILD", "false")
        .env("KNIXL_NIX", &ok_eval)
        .env("KNIXL_NIX_BUILD", &ok_build)
        .env("KNIXL_PIN_RESOLVER", &pin_resolver)
        .env("KNIXL_BASELINE_RESOLVER", &baseline_resolver)
        .output()
        .expect("run knixl install");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("25.05") && stdout.contains("deadbeef"),
        "status line: {stdout}"
    );

    let lock = fs::read_to_string(root.join("knixl.lock.kdl")).unwrap();
    assert!(
        lock.contains("baseline release=\"25.05\""),
        "baseline recorded: {lock}"
    );
    assert!(
        lock.contains("nixpkgs-rev=\"deadbeef\""),
        "resolved rev recorded: {lock}"
    );

    let _ = fs::remove_file(&ok_eval);
    let _ = fs::remove_file(&pin_resolver);
    let _ = fs::remove_file(&baseline_resolver);
    let _ = fs::remove_file(&ok_build);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn install_ambient_with_a_declared_release_writes_the_baseline() {
    // issue #22 review fix: the baseline used to be resolved and written by a pre-pass for
    // every host regardless of command, so an ambient (unversioned) install never exercised
    // the write itself; now `commit_install` writes it for ambient installs too.
    let root = temp_project("install-baseline-ambient");
    declare_baseline_release(&root, "25.05");

    let ok_eval = nix_shim("baseline-ambient-eval", true); // package resolves + parses
    let baseline_resolver = resolver_shim("baseline-ambient-rev", "deadbeef", "", 0);

    let out = Command::new(env!("CARGO_BIN_EXE_knixl"))
        .args(["install", "ripgrep", "--host", "web", "--yes"])
        .current_dir(&root)
        .env("KNIXL_FORMATTER", "cat")
        .env("KNIXL_NIX_BUILD", "false")
        .env("KNIXL_NIX", &ok_eval)
        .env("KNIXL_BASELINE_RESOLVER", &baseline_resolver)
        .output()
        .expect("run knixl install");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let lock = fs::read_to_string(root.join("knixl.lock.kdl")).unwrap();
    assert!(
        lock.contains("baseline release=\"25.05\""),
        "baseline recorded: {lock}"
    );
    assert!(
        lock.contains("nixpkgs-rev=\"deadbeef\""),
        "resolved rev recorded: {lock}"
    );

    let _ = fs::remove_file(&ok_eval);
    let _ = fs::remove_file(&baseline_resolver);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn install_reverts_the_baseline_when_commit_fails_after_write_baseline() {
    // Mirrors `install_pkg_at_version_reverts_the_pin_when_commit_fails_after_write_pin`,
    // but for the baseline write (issue #22 review fix): a baseline resolved during a
    // confirmed install must not linger if the commit fails after it was written.
    let root = temp_project("install-baseline-revert");
    declare_baseline_release(&root, "25.05");

    let parse_fail_eval = nix_shim_parse_fails("baseline-revert");
    let baseline_resolver = resolver_shim("baseline-revert-rev", "deadbeef", "", 0);

    let out = Command::new(env!("CARGO_BIN_EXE_knixl"))
        .args(["install", "ripgrep", "--host", "web", "--yes"])
        .current_dir(&root)
        .env("KNIXL_FORMATTER", "cat")
        .env("KNIXL_NIX_BUILD", "false")
        .env("KNIXL_NIX", &parse_fail_eval)
        .env("KNIXL_BASELINE_RESOLVER", &baseline_resolver)
        .output()
        .expect("run knixl install");
    assert_eq!(
        out.status.code(),
        Some(5),
        "parse failure after the baseline write: {out:?}"
    );

    let kdl = fs::read_to_string(root.join("hosts/web.kdl")).unwrap();
    assert!(!kdl.contains("ripgrep"), "kdl reverted: {kdl}");

    let lock_path = root.join("knixl.lock.kdl");
    if lock_path.exists() {
        let lock = fs::read_to_string(&lock_path).unwrap();
        assert!(
            !lock.contains("baseline"),
            "baseline not left dangling after revert: {lock}"
        );
    }

    let _ = fs::remove_file(&parse_fail_eval);
    let _ = fs::remove_file(&baseline_resolver);
    let _ = fs::remove_dir_all(&root);
}

/// #35 phase 3: `install` resolves a project's declared `oracle-modules` (via
/// `KNIXL_MODULE_RESOLVER`, mirroring the baseline resolver shim) and records the pin in the
/// lock, in the same committed step as the package install.
#[test]
fn install_with_a_declared_oracle_module_resolves_and_writes_the_pin() {
    let root = temp_project("install-oracle-module");
    declare_project_oracle_module(&root, "disko", "github:nix-community/disko");

    let ok_eval = nix_shim("oracle-module-eval", true); // package resolves + parses
    let module_resolver = resolver_shim("oracle-module-rev", "deadbeef", "", 0);

    let out = Command::new(env!("CARGO_BIN_EXE_knixl"))
        .args(["install", "ripgrep", "--host", "web", "--yes"])
        .current_dir(&root)
        .env("KNIXL_FORMATTER", "cat")
        .env("KNIXL_NIX_BUILD", "false")
        .env("KNIXL_NIX", &ok_eval)
        .env("KNIXL_MODULE_RESOLVER", &module_resolver)
        .output()
        .expect("run knixl install");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("disko") && stdout.contains("deadbeef"),
        "status line: {stdout}"
    );

    let lock = fs::read_to_string(root.join("knixl.lock.kdl")).unwrap();
    assert!(
        lock.contains("oracle-module name=\"disko\""),
        "module pin recorded: {lock}"
    );
    assert!(
        lock.contains("url=\"https://github.com/nix-community/disko\""),
        "resolved url recorded: {lock}"
    );
    assert!(
        lock.contains("rev=\"deadbeef\""),
        "resolved rev recorded: {lock}"
    );
    assert!(
        lock.contains("attr=\"default\""),
        "default attr recorded: {lock}"
    );

    let _ = fs::remove_file(&ok_eval);
    let _ = fs::remove_file(&module_resolver);
    let _ = fs::remove_dir_all(&root);
}

/// A failed/cancelled install must restore the lock's prior `oracle.modules`, never blank it
/// and never leave the newly resolved value in its place. Mirrors
/// `install_reverts_the_baseline_when_commit_fails_after_write_baseline`, but seeds an
/// existing pin first so the revert has a genuine prior value distinct from both "cleared"
/// and "the new pin".
#[test]
fn install_reverts_the_oracle_modules_to_their_prior_value_when_commit_fails_after_write() {
    let root = temp_project("install-oracle-module-revert");
    assert_eq!(knixl(&root, &["generate"]).status.code(), Some(0));

    declare_project_oracle_module(&root, "disko", "github:nix-community/disko");
    let seed_resolver = resolver_shim("oracle-module-revert-seed", "priorrev", "", 0);
    let seed = Command::new(env!("CARGO_BIN_EXE_knixl"))
        .args(["upgrade", "--yes"])
        .current_dir(&root)
        .env("KNIXL_FORMATTER", "cat")
        .env("KNIXL_NIX_BUILD", "false")
        .env("KNIXL_MODULE_RESOLVER", &seed_resolver)
        .output()
        .expect("run knixl upgrade --yes (seed)");
    assert_eq!(
        seed.status.code(),
        Some(0),
        "seed upgrade: {}",
        String::from_utf8_lossy(&seed.stderr)
    );
    let lock_before = fs::read_to_string(root.join("knixl.lock.kdl")).unwrap();
    assert!(
        lock_before.contains("oracle-module name=\"disko\"")
            && lock_before.contains("rev=\"priorrev\""),
        "prior pin seeded: {lock_before}"
    );

    // Change the declared module so it no longer matches what is pinned: a fresh resolution
    // is now pending, and it must replace (in memory) the prior pin above.
    declare_project_oracle_module(&root, "impermanence", "github:nix-community/impermanence");
    let new_resolver = resolver_shim("oracle-module-revert-new", "newrev", "", 0);
    let parse_fail_eval = nix_shim_parse_fails("oracle-module-revert");

    let out = Command::new(env!("CARGO_BIN_EXE_knixl"))
        .args(["install", "ripgrep", "--host", "web", "--yes"])
        .current_dir(&root)
        .env("KNIXL_FORMATTER", "cat")
        .env("KNIXL_NIX_BUILD", "false")
        .env("KNIXL_NIX", &parse_fail_eval)
        .env("KNIXL_MODULE_RESOLVER", &new_resolver)
        .output()
        .expect("run knixl install");
    assert_eq!(
        out.status.code(),
        Some(5),
        "parse failure after the module write: {out:?}"
    );

    let kdl = fs::read_to_string(root.join("hosts/web.kdl")).unwrap();
    assert!(!kdl.contains("ripgrep"), "kdl reverted: {kdl}");

    let lock_after = fs::read_to_string(root.join("knixl.lock.kdl")).unwrap();
    assert!(
        lock_after.contains("oracle-module name=\"disko\"")
            && lock_after.contains("rev=\"priorrev\""),
        "prior pin restored (not blanked, not the new value): {lock_after}"
    );
    assert!(
        !lock_after.contains("impermanence") && !lock_after.contains("newrev"),
        "new pin not left dangling after revert: {lock_after}"
    );

    let _ = fs::remove_file(&seed_resolver);
    let _ = fs::remove_file(&new_resolver);
    let _ = fs::remove_file(&parse_fail_eval);
    let _ = fs::remove_dir_all(&root);
}

/// ADR 0008: a host `oracle-modules` override requires a declared `nixpkgs release=`; refused
/// otherwise.
#[test]
fn install_refuses_a_host_oracle_modules_override_without_a_declared_release() {
    let root = temp_project("host-oracle-module-no-release");
    declare_host_oracle_module(&root, "web", "disko", "github:nix-community/disko");

    // A resolver that would fail the test if it were ever invoked: the run must be refused
    // before any resolution is attempted.
    let never_called = resolver_shim(
        "host-oracle-module-no-release-never",
        "",
        "should not be called",
        1,
    );
    let out = Command::new(env!("CARGO_BIN_EXE_knixl"))
        .args(["install", "ripgrep", "--host", "web", "--yes"])
        .current_dir(&root)
        .env("KNIXL_FORMATTER", "cat")
        .env("KNIXL_NIX_BUILD", "false")
        .env("KNIXL_MODULE_RESOLVER", &never_called)
        .output()
        .expect("run knixl install");
    assert_eq!(
        out.status.code(),
        Some(5),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("web")
            && stderr.contains("oracle-modules")
            && stderr.contains("nixpkgs release"),
        "stderr: {stderr}"
    );

    // Refused before any write: no draft left on the host, no lock created.
    let kdl = fs::read_to_string(root.join("hosts/web.kdl")).unwrap();
    assert!(!kdl.contains("ripgrep"), "no draft left behind: {kdl}");

    let _ = fs::remove_file(&never_called);
    let _ = fs::remove_dir_all(&root);
}

/// A host that declares both a `nixpkgs release=` and its own `oracle-modules` override
/// writes the module pin to its own `baseline.modules` (ADR 0008); the project-wide
/// `oracle.modules` stays empty here. Mirrors
/// `install_with_a_declared_oracle_module_resolves_and_writes_the_pin`, with a host-level
/// override and a baseline resolved in the same run.
#[test]
fn install_with_a_declared_host_oracle_module_resolves_and_writes_it_to_the_baseline() {
    let root = temp_project("install-host-oracle-module");
    declare_baseline_release(&root, "25.05");
    declare_host_oracle_module(&root, "web", "disko", "github:nix-community/disko");

    let ok_eval = nix_shim("host-oracle-module-eval", true); // package resolves + parses
    let baseline_resolver = resolver_shim("host-oracle-module-baseline", "deadbeef", "", 0);
    let module_resolver = resolver_shim("host-oracle-module-rev", "modrev123", "", 0);
    // The host now has a real (non-empty) module pin, so `commit_install` attempts the
    // augmented-set build (ADR 0008): shim it to succeed, so this test stays about the
    // resolve-and-write wiring rather than the build itself (covered separately below).
    let build_shim = optionsdoc_build_shim(
        "host-oracle-module-resolve",
        r#"{ "disko.devices.disk.main.device": { "type": "string" } }"#,
    );
    let xdg = std::env::temp_dir().join(format!(
        "knixl-cli-host-oracle-module-xdg-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&xdg);

    let out = Command::new(env!("CARGO_BIN_EXE_knixl"))
        .args(["install", "ripgrep", "--host", "web", "--yes"])
        .current_dir(&root)
        .env("KNIXL_FORMATTER", "cat")
        .env("KNIXL_NIX_BUILD", "false")
        .env("KNIXL_NIX", &ok_eval)
        .env("KNIXL_BASELINE_RESOLVER", &baseline_resolver)
        .env("KNIXL_MODULE_RESOLVER", &module_resolver)
        .env("KNIXL_NIX_BUILD", &build_shim)
        .env("XDG_CACHE_HOME", &xdg)
        .output()
        .expect("run knixl install");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("disko") && stdout.contains("modrev123") && stdout.contains("web"),
        "status line: {stdout}"
    );

    let lock = fs::read_to_string(root.join("knixl.lock.kdl")).unwrap();
    assert!(
        lock.contains("baseline release=\"25.05\""),
        "baseline recorded: {lock}"
    );
    assert!(
        lock.contains("rev=\"modrev123\""),
        "resolved module rev recorded: {lock}"
    );
    // Exactly one "disko" pin in the whole lock: the host's own, not a project-wide one (the
    // project here declares no `oracle-modules` at all).
    assert_eq!(
        lock.matches("oracle-module name=\"disko\"").count(),
        1,
        "got: {lock}"
    );

    let _ = fs::remove_file(&ok_eval);
    let _ = fs::remove_file(&baseline_resolver);
    let _ = fs::remove_file(&module_resolver);
    let _ = fs::remove_file(&build_shim);
    let _ = fs::remove_dir_all(&xdg);
    let _ = fs::remove_dir_all(&root);
}
