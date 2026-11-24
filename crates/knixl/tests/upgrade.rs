//! `knixl upgrade` and `knixl updates` end to end.

mod common;

use std::fs;
use std::process::Command;

use common::*;

/// #135: in input mode every host shares one nixpkgs, so an upgrade that resolves a second
/// host onto another release must refuse before writing anything. It used to plan off the
/// old lock, miss the conflict, drop the flake from the lock and exit 0.
#[test]
fn upgrade_refuses_a_second_baseline_in_input_mode() {
    use std::os::unix::fs::PermissionsExt;
    let root = temp_project("input-mode-two-baselines");
    fs::remove_file(root.join("hosts/db.kdl")).unwrap();
    fs::write(
        root.join("knixl.kdl"),
        "system {\n    state-version \"25.11\"\n    input \"nixpkgs\" url=\"github:NixOS/nixpkgs\"\n}\n",
    )
    .unwrap();
    declare_baseline_release(&root, "25.11");
    let resolver =
        std::env::temp_dir().join(format!("knixl-cli-two-baselines-{}", std::process::id()));
    fs::write(
        &resolver,
        "#!/bin/sh\ncase \"$1\" in\n25.11) echo 1111111111111111111111111111111111111111 ;;\n*) echo 2222222222222222222222222222222222222222 ;;\nesac\n",
    )
    .unwrap();
    fs::set_permissions(&resolver, fs::Permissions::from_mode(0o755)).unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_knixl"))
            .args(args)
            .current_dir(&root)
            .env("KNIXL_FORMATTER", "cat")
            .env("KNIXL_NIX_BUILD", "false")
            .env("KNIXL_BASELINE_RESOLVER", &resolver)
            .output()
            .unwrap()
    };
    assert_eq!(run(&["upgrade", "--yes"]).status.code(), Some(0));
    assert_eq!(run(&["generate"]).status.code(), Some(0));
    let lock_before = fs::read_to_string(root.join("knixl.lock.kdl")).unwrap();

    fs::write(
        root.join("hosts/nas.kdl"),
        "host \"nas\" {\n    system \"x86_64-linux\"\n    nixpkgs release=\"25.05\"\n}\n",
    )
    .unwrap();
    let out = run(&["upgrade", "--yes"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(5), "{stderr}");
    assert!(stderr.contains("one nixpkgs baseline rev"), "{stderr}");
    assert_eq!(
        fs::read_to_string(root.join("knixl.lock.kdl")).unwrap(),
        lock_before,
        "a refused upgrade writes nothing"
    );
    assert!(!root.join("generated/hosts/nas.nix").exists());
    let _ = fs::remove_file(&resolver);
    let _ = fs::remove_dir_all(&root);
}

/// #128: a host whose nixpkgs has no cached option set is reported, and `upgrade` builds the
/// set (here with a fake nix-build that writes a tiny options.json), after which it validates.
#[test]
fn upgrade_builds_a_missing_base_option_set() {
    use std::os::unix::fs::PermissionsExt;
    let root = temp_project("base-option-set");
    let cache = root.join("xdg-cache");
    let out_dir = root.join("fake-store");
    fs::create_dir_all(out_dir.join("share/doc/nixos")).unwrap();
    fs::write(
        out_dir.join("share/doc/nixos/options.json"),
        r#"{"services.nginx.enable":{"type":"boolean","readOnly":false}}"#,
    )
    .unwrap();
    let build = root.join("fake-nix-build");
    fs::write(&build, format!("#!/bin/sh\necho {}\n", out_dir.display())).unwrap();
    fs::set_permissions(&build, fs::Permissions::from_mode(0o755)).unwrap();
    let resolver = resolver_shim("base-option-set-rev", "cafe", "", 0);
    declare_baseline_release(&root, "25.11");
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_knixl"))
            .args(args)
            .current_dir(&root)
            .env("KNIXL_FORMATTER", "cat")
            .env("KNIXL_NIX_BUILD", &build)
            .env("KNIXL_BASELINE_RESOLVER", &resolver)
            .env("XDG_CACHE_HOME", &cache)
            .output()
            .unwrap()
    };
    // Resolving the release builds its set straight away.
    let up = run(&["upgrade", "--yes"]);
    assert_eq!(
        up.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&up.stderr)
    );
    let cached = cache.join("knixl/options-cafe.json");
    assert!(cached.is_file(), "the base set is cached");

    // With the cache gone, plan says so and upgrade offers to rebuild it.
    fs::remove_file(&cached).unwrap();
    let plan = run(&["plan"]);
    assert!(
        String::from_utf8_lossy(&plan.stderr).contains("no NixOS option set for nixpkgs cafe"),
        "{}",
        String::from_utf8_lossy(&plan.stderr)
    );
    let preview = run(&["upgrade"]);
    assert_eq!(preview.status.code(), Some(4));
    assert!(String::from_utf8_lossy(&preview.stdout)
        .contains("would build the NixOS option set for nixpkgs cafe"));
    assert_eq!(run(&["upgrade", "--yes"]).status.code(), Some(0));
    assert!(cached.is_file(), "rebuilt");
    let plan = run(&["plan"]);
    assert!(!String::from_utf8_lossy(&plan.stderr).contains("no NixOS option set"));
    let _ = fs::remove_file(&resolver);
    let _ = fs::remove_dir_all(&root);
}

/// #134: a project-level `nixpkgs release` resolves into the lock's oracle line on upgrade,
/// and dropping it clears the line again.
#[test]
fn upgrade_resolves_and_clears_the_project_release() {
    let root = temp_project("project-release");
    let resolver = resolver_shim("project-release-rev", "feed", "", 0);
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_knixl"))
            .args(args)
            .current_dir(&root)
            .env("KNIXL_FORMATTER", "cat")
            .env("KNIXL_NIX_BUILD", "false")
            .env("KNIXL_BASELINE_RESOLVER", &resolver)
            .output()
            .unwrap()
    };
    let lock = || fs::read_to_string(root.join("knixl.lock.kdl")).unwrap();
    assert_eq!(run(&["generate"]).status.code(), Some(0));
    fs::write(root.join("knixl.kdl"), "nixpkgs release=\"25.11\"\n").unwrap();

    let preview = run(&["upgrade"]);
    assert_eq!(preview.status.code(), Some(4));
    assert!(String::from_utf8_lossy(&preview.stdout)
        .contains("would resolve the project nixpkgs release \"25.11\" -> feed"));
    assert!(
        lock().contains("oracle nixpkgs-rev=\"\""),
        "a preview writes nothing"
    );

    assert_eq!(run(&["upgrade", "--yes"]).status.code(), Some(0));
    assert!(
        lock().contains("oracle release=\"25.11\" nixpkgs-rev=\"feed\""),
        "{}",
        lock()
    );
    // Resolved once; the (failed, fake) option-set build is all that is still offered.
    assert!(!String::from_utf8_lossy(&run(&["upgrade"]).stdout).contains("project nixpkgs release"));

    fs::remove_file(root.join("knixl.kdl")).unwrap();
    assert_eq!(run(&["upgrade", "--yes"]).status.code(), Some(0));
    assert!(lock().contains("oracle nixpkgs-rev=\"\""), "{}", lock());
    let _ = fs::remove_file(&resolver);
    let _ = fs::remove_dir_all(&root);
}

/// #129: a resolved release stays on its commit until `upgrade --refresh` moves it to the
/// branch's new tip; an exact `rev=` never moves.
#[test]
fn upgrade_refresh_moves_a_release_to_its_branch_tip() {
    use std::os::unix::fs::PermissionsExt;
    let root = temp_project("refresh");
    let tip = root.join("branch-tip");
    fs::write(&tip, "aaaa\n").unwrap();
    let resolver = root.join("resolver");
    fs::write(&resolver, format!("#!/bin/sh\ncat {}\n", tip.display())).unwrap();
    fs::set_permissions(&resolver, fs::Permissions::from_mode(0o755)).unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_knixl"))
            .args(args)
            .current_dir(&root)
            .env("KNIXL_FORMATTER", "cat")
            .env("KNIXL_NIX_BUILD", "false")
            .env("KNIXL_BASELINE_RESOLVER", &resolver)
            .output()
            .unwrap()
    };
    let lock = || fs::read_to_string(root.join("knixl.lock.kdl")).unwrap();
    declare_baseline_release(&root, "25.11");
    assert_eq!(run(&["upgrade", "--yes"]).status.code(), Some(0));
    assert!(lock().contains("nixpkgs-rev=\"aaaa\""));

    // The branch moves on; a plain upgrade leaves the host where it was.
    fs::write(&tip, "bbbb\n").unwrap();
    let plain = run(&["upgrade"]);
    assert!(!String::from_utf8_lossy(&plain.stdout).contains("would resolve nixpkgs release"));

    let preview = run(&["upgrade", "--refresh"]);
    assert!(
        String::from_utf8_lossy(&preview.stdout)
            .contains("would resolve nixpkgs release \"25.11\" for host \"web\" -> bbbb"),
        "{}",
        String::from_utf8_lossy(&preview.stdout)
    );
    assert!(
        lock().contains("nixpkgs-rev=\"aaaa\""),
        "a preview writes nothing"
    );
    assert_eq!(
        run(&["upgrade", "--refresh", "--yes"]).status.code(),
        Some(0)
    );
    assert!(lock().contains("nixpkgs-rev=\"bbbb\""), "{}", lock());

    // Nothing to move: --refresh is a no-op for the baseline.
    let again = run(&["upgrade", "--refresh"]);
    assert!(!String::from_utf8_lossy(&again.stdout).contains("would resolve nixpkgs release"));
    let _ = fs::remove_dir_all(&root);
}

/// #142: `updates` evaluates hosts through the generated flake, so a modules-only project
/// is told what it needs instead of failing somewhere inside nix.
#[test]
fn updates_needs_a_generated_flake() {
    let root = temp_project("updates-no-flake");
    let out = knixl(&root, &["updates"]);
    assert_eq!(out.status.code(), Some(5));
    assert!(String::from_utf8_lossy(&out.stderr).contains("add a system block"));
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn upgrade_prints_migration_notes_for_a_module_bump() {
    let root = temp_project("upgrade");
    assert_eq!(knixl(&root, &["generate"]).status.code(), Some(0));

    // Record an older web-service in the lock: the manifest is 1.2.0, so this is version skew.
    let lock_path = root.join("knixl.lock.kdl");
    let lock = fs::read_to_string(&lock_path).unwrap().replace(
        "module \"web-service\" version=\"1.2.0\"",
        "module \"web-service\" version=\"1.0.0\"",
    );
    fs::write(&lock_path, lock).unwrap();
    // Make one output Missing so the skew forces the ack path (upgrade, not "up to date").
    fs::remove_file(root.join("generated/hosts/web.nix")).unwrap();

    let out = knixl(&root, &["upgrade"]);
    // No --yes: skew needs a human, exit 4 (NeedsAck).
    assert_eq!(
        out.status.code(),
        Some(4),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("web-service 1.0.0 -> 1.2.0"),
        "notes header: {stdout}"
    );
    assert!(stdout.contains("enableACME"), "1.1.0 note shown: {stdout}");
    assert!(
        stdout.contains("serverAliases"),
        "1.2.0 note shown: {stdout}"
    );
    let _ = fs::remove_dir_all(&root);
}

/// issue #22 review fix: `upgrade` without `--yes` must preview a pending baseline
/// resolution (a network lookup, but read-only) and never write it, and must not report
/// "already up to date" when there is a baseline pending.
#[test]
fn upgrade_without_yes_previews_a_pending_baseline_without_writing_the_lock() {
    let root = temp_project("upgrade-baseline-preview");
    assert_eq!(knixl(&root, &["generate"]).status.code(), Some(0));
    declare_baseline_release(&root, "25.05");

    let lock_path = root.join("knixl.lock.kdl");
    let lock_before = fs::read_to_string(&lock_path).unwrap();

    let baseline_resolver = resolver_shim("upgrade-preview-rev", "deadbeef", "", 0);
    let out = Command::new(env!("CARGO_BIN_EXE_knixl"))
        .args(["upgrade"])
        .current_dir(&root)
        .env("KNIXL_FORMATTER", "cat")
        .env("KNIXL_NIX_BUILD", "false")
        .env("KNIXL_BASELINE_RESOLVER", &baseline_resolver)
        .output()
        .expect("run knixl upgrade");
    assert_eq!(
        out.status.code(),
        Some(4),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("already up to date"),
        "a pending baseline is work to do: {stdout}"
    );
    assert!(
        stdout.contains("would resolve") && stdout.contains("25.05") && stdout.contains("deadbeef"),
        "preview line: {stdout}"
    );

    let lock_after = fs::read_to_string(&lock_path).unwrap();
    assert_eq!(lock_before, lock_after, "no --yes must not touch the lock");

    let _ = fs::remove_file(&baseline_resolver);
    let _ = fs::remove_dir_all(&root);
}

/// issue #22 review fix: `upgrade --yes` commits a pending baseline resolution to the lock.
#[test]
fn upgrade_with_yes_writes_a_pending_baseline() {
    let root = temp_project("upgrade-baseline-apply");
    assert_eq!(knixl(&root, &["generate"]).status.code(), Some(0));
    declare_baseline_release(&root, "25.05");

    let baseline_resolver = resolver_shim("upgrade-apply-rev", "deadbeef", "", 0);
    let out = Command::new(env!("CARGO_BIN_EXE_knixl"))
        .args(["upgrade", "--yes"])
        .current_dir(&root)
        .env("KNIXL_FORMATTER", "cat")
        .env("KNIXL_NIX_BUILD", "false")
        .env("KNIXL_BASELINE_RESOLVER", &baseline_resolver)
        .output()
        .expect("run knixl upgrade --yes");
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

    let _ = fs::remove_file(&baseline_resolver);
    let _ = fs::remove_dir_all(&root);
}

/// #35 phase 3: `upgrade --yes` resolves and commits a project's declared `oracle-modules`,
/// mirroring `upgrade_with_yes_writes_a_pending_baseline`.
#[test]
fn upgrade_with_yes_writes_a_pending_oracle_module() {
    let root = temp_project("upgrade-oracle-module");
    assert_eq!(knixl(&root, &["generate"]).status.code(), Some(0));
    declare_project_oracle_module(&root, "disko", "github:nix-community/disko");

    let module_resolver = resolver_shim("upgrade-oracle-module-rev", "deadbeef", "", 0);
    let out = Command::new(env!("CARGO_BIN_EXE_knixl"))
        .args(["upgrade", "--yes"])
        .current_dir(&root)
        .env("KNIXL_FORMATTER", "cat")
        .env("KNIXL_NIX_BUILD", "false")
        .env("KNIXL_MODULE_RESOLVER", &module_resolver)
        .output()
        .expect("run knixl upgrade --yes");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let lock = fs::read_to_string(root.join("knixl.lock.kdl")).unwrap();
    assert!(
        lock.contains("oracle-module name=\"disko\""),
        "module pin recorded: {lock}"
    );
    assert!(
        lock.contains("rev=\"deadbeef\""),
        "resolved rev recorded: {lock}"
    );

    let _ = fs::remove_file(&module_resolver);
    let _ = fs::remove_dir_all(&root);
}

/// A project that deletes its declared `oracle-modules` block must GC the lock's stale pins,
/// not leave them forever. Mirrors `upgrade_with_yes_writes_a_pending_oracle_module`, but
/// removes the declaration on a second run instead of adding one.
#[test]
fn upgrade_with_yes_clears_oracle_modules_when_the_declaration_is_removed() {
    let root = temp_project("upgrade-oracle-module-removed");
    assert_eq!(knixl(&root, &["generate"]).status.code(), Some(0));
    declare_project_oracle_module(&root, "disko", "github:nix-community/disko");

    let module_resolver = resolver_shim("upgrade-oracle-module-removed-rev", "deadbeef", "", 0);
    let seed = Command::new(env!("CARGO_BIN_EXE_knixl"))
        .args(["upgrade", "--yes"])
        .current_dir(&root)
        .env("KNIXL_FORMATTER", "cat")
        .env("KNIXL_NIX_BUILD", "false")
        .env("KNIXL_MODULE_RESOLVER", &module_resolver)
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
        lock_before.contains("oracle-module name=\"disko\""),
        "pin seeded: {lock_before}"
    );

    // The project no longer declares any oracle modules.
    fs::remove_file(root.join("knixl.kdl")).unwrap();

    // The resolver must never be called for a removal: clearing the pins needs no network
    // lookup, only a write of the (now empty) set.
    let never_called = resolver_shim(
        "upgrade-oracle-module-removed-never",
        "",
        "should not be called",
        1,
    );
    let out = Command::new(env!("CARGO_BIN_EXE_knixl"))
        .args(["upgrade", "--yes"])
        .current_dir(&root)
        .env("KNIXL_FORMATTER", "cat")
        .env("KNIXL_NIX_BUILD", "false")
        .env("KNIXL_MODULE_RESOLVER", &never_called)
        .output()
        .expect("run knixl upgrade --yes");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let lock_after = fs::read_to_string(root.join("knixl.lock.kdl")).unwrap();
    assert!(
        !lock_after.contains("oracle-module"),
        "stale pin GC'd after declaration removed: {lock_after}"
    );

    let _ = fs::remove_file(&module_resolver);
    let _ = fs::remove_file(&never_called);
    let _ = fs::remove_dir_all(&root);
}

/// End-to-end (ADR 0008): `upgrade --yes` resolves a host's declared release and its own
/// `oracle-modules` override, BUILDS the augmented `options.json` for that effective set (via
/// a shimmed `nix-build`, so this needs no real nix), caches it, and records the built
/// content's hash as the host's `options-hash`. A later `check` then validates a host emitting
/// a `disko.*` path cleanly against that cached, augmented set.
#[test]
fn upgrade_builds_and_caches_the_augmented_set_and_a_later_check_validates_disko_paths() {
    let root = temp_project("host-oracle-module-build");
    write_disko_test_module(&root);
    fs::write(
        root.join("hosts/diskohost.kdl"),
        "host \"diskohost\" {\n    system \"x86_64-linux\"\n    nixpkgs release=\"25.05\"\n    oracle-modules {\n        module \"disko\" flake=\"github:nix-community/disko\"\n    }\n    disko-test\n}\n",
    )
    .unwrap();

    let baseline_resolver = resolver_shim("host-oracle-module-build-baseline", "deadbeef", "", 0);
    let module_resolver = resolver_shim("host-oracle-module-build-rev", "modrev123", "", 0);
    let build_shim = optionsdoc_build_shim(
        "host-oracle-module-build",
        r#"{ "nixpkgs.hostPlatform": { "type": "string" }, "networking.hostName": { "type": "string" }, "disko.devices.disk.main.device": { "type": "string" } }"#,
    );
    let xdg = std::env::temp_dir().join(format!("knixl-cli-optionsdoc-xdg-{}", std::process::id()));
    let _ = fs::remove_dir_all(&xdg);

    let out = Command::new(env!("CARGO_BIN_EXE_knixl"))
        .args(["upgrade", "--yes"])
        .current_dir(&root)
        .env("KNIXL_FORMATTER", "cat")
        .env("KNIXL_NIX_BUILD", "false")
        .env("KNIXL_BASELINE_RESOLVER", &baseline_resolver)
        .env("KNIXL_MODULE_RESOLVER", &module_resolver)
        .env("KNIXL_NIX_BUILD", &build_shim)
        .env("XDG_CACHE_HOME", &xdg)
        .output()
        .expect("run knixl upgrade --yes");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let lock = fs::read_to_string(root.join("knixl.lock.kdl")).unwrap();
    assert!(
        lock.contains("oracle-module name=\"disko\""),
        "module pin recorded: {lock}"
    );
    let baseline_line = lock
        .lines()
        .find(|l| l.contains("baseline release=\"25.05\""))
        .unwrap_or_else(|| panic!("no baseline line: {lock}"));
    assert!(
        !baseline_line.contains("options-hash=\"\""),
        "options-hash recorded from the built content: {baseline_line}"
    );

    // A later `check`, with no build shim needed this time, validates cleanly: it loads the
    // AUGMENTED set (rev + disko pin) this run just built and cached, not the (nonexistent)
    // base-only set.
    let check = Command::new(env!("CARGO_BIN_EXE_knixl"))
        .args(["check"])
        .current_dir(&root)
        .env("KNIXL_FORMATTER", "cat")
        .env("KNIXL_NIX_BUILD", "false")
        .env("XDG_CACHE_HOME", &xdg)
        .output()
        .expect("run knixl check");
    assert_eq!(
        check.status.code(),
        Some(0),
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&check.stderr)
    );

    let _ = fs::remove_file(&baseline_resolver);
    let _ = fs::remove_file(&module_resolver);
    let _ = fs::remove_file(&build_shim);
    let _ = fs::remove_dir_all(&xdg);
    let _ = fs::remove_dir_all(&root);
}

/// ADR 0014: adding a flake input to an already generated input-mode project. `upgrade` has to
/// plan the flake from the newly resolved pins; planning from the old lock left the flake out of
/// the expected set, so its lock entry was dropped and the next `generate` called it drift.
#[test]
fn upgrade_adding_a_flake_input_rewrites_the_flake_and_generate_stays_clean() {
    let root = std::env::temp_dir().join(format!("knixl-cli-{}-flake-input", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("hosts")).unwrap();
    let rev = |c: char| c.to_string().repeat(40);
    let project = |extra: &str| {
        format!(
            "system {{\n    state-version \"25.11\"\n    input \"nixpkgs\" url=\"github:NixOS/nixpkgs\"\n{extra}}}\n"
        )
    };
    fs::write(root.join("knixl.kdl"), project("")).unwrap();
    fs::write(
        root.join("hosts/box.kdl"),
        format!(
            "host \"box\" {{\n    system \"x86_64-linux\"\n    nixpkgs release=\"unstable\" rev=\"{}\"\n}}\n",
            rev('a')
        ),
    )
    .unwrap();

    let up = knixl(&root, &["upgrade", "--yes"]);
    assert_eq!(
        up.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&up.stderr)
    );
    let gen = knixl(&root, &["generate"]);
    assert_eq!(
        gen.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&gen.stderr)
    );

    fs::write(
        root.join("knixl.kdl"),
        project(&format!(
            "    input \"disko\" url=\"github:nix-community/disko\" rev=\"{}\"\n",
            rev('b')
        )),
    )
    .unwrap();
    assert_eq!(
        knixl(&root, &["generate"]).status.code(),
        Some(5),
        "unresolved input refuses"
    );

    let up = knixl(&root, &["upgrade", "--yes"]);
    assert_eq!(
        up.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&up.stderr)
    );
    let flake = fs::read_to_string(root.join("generated/flake.nix")).unwrap();
    assert!(
        flake.contains(&format!("github:nix-community/disko/{}", rev('b'))),
        "upgrade wrote the new flake: {flake}"
    );
    let gen = knixl(&root, &["generate"]);
    assert_eq!(
        gen.status.code(),
        Some(0),
        "no drift after upgrade: {}",
        String::from_utf8_lossy(&gen.stderr)
    );

    let _ = fs::remove_dir_all(&root);
}
