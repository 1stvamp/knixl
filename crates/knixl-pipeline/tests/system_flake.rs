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
fn installer_emits_a_module_file_and_an_iso_flake_output() {
    let root = temp_root("installer");
    fs::write(
        root.join("knixl.kdl"),
        "system {\n    state-version \"25.05\"\n}\ninstaller \"usb\" system=\"x86_64-linux\" {\n    os {\n        state-version \"25.05\"\n    }\n    openssh {\n    }\n}\n",
    )
    .unwrap();
    fs::write(
        root.join("hosts/web.kdl"),
        "host \"web\" {\n    system \"x86_64-linux\"\n    nixpkgs release=\"25.05\"\n}\n",
    )
    .unwrap();

    let formatter = identity_formatter();
    let tool: semver::Version = "0.3.1".parse().unwrap();

    // Seed a lock with a resolved host baseline (so the flake emits) and an oracle rev (which
    // installers pin to).
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
    lock.oracle.nixpkgs_rev = "installerrev0987".into();
    fs::write(root.join("knixl.lock.kdl"), lock.render()).unwrap();

    let project = gather(&root, &formatter, tool).expect("gather");

    // The installer module file: modulesPath formals + installation-cd import + the re-used
    // module tree at top level (not re-rooted, unlike a guest).
    let installer_path = PathBuf::from("generated/installer/usb.nix");
    let module = project.generated.get(&installer_path).unwrap_or_else(|| {
        panic!(
            "generated/installer/usb.nix missing: {:?}",
            project.generated.keys().collect::<Vec<_>>()
        )
    });
    assert!(
        module.contains("modulesPath"),
        "installer formals: {module}"
    );
    // Parenthesised: a bare `modulesPath + "..."` is not a valid list element (#81).
    assert!(
        module.contains("(modulesPath + \"/installer/cd-dvd/installation-cd-minimal.nix\")"),
        "installation-cd import: {module}"
    );
    assert!(
        module.contains("services.openssh.enable = true"),
        "module tree lowered: {module}"
    );
    assert!(
        module.contains("nixpkgs.hostPlatform = \"x86_64-linux\""),
        "hostPlatform: {module}"
    );

    // The flake: the ISO package output pinned to the oracle rev.
    let flake = project
        .generated
        .get(&PathBuf::from("generated/flake.nix"))
        .expect("flake present");
    assert!(
        flake.contains("installerrev0987"),
        "installer pinned to oracle rev: {flake}"
    );
    assert!(flake.contains("usb-iso"), "iso package output: {flake}");
    assert!(flake.contains("isoImage"), "iso build attr: {flake}");

    // The installer module file rides the lock's expected outputs like any generated file.
    let plan = Plan::compute(
        &project.inputs,
        &project.disk,
        &project.lock,
        &project.versions,
    );
    assert!(
        plan.lock_next
            .outputs
            .iter()
            .any(|o| o.path == installer_path),
        "lock outputs missing the installer module"
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn guest_image_emits_an_lxc_module_and_flake_outputs() {
    let root = temp_root("guest-image");
    fs::write(
        root.join("knixl.kdl"),
        "system {\n    state-version \"25.05\"\n}\nguest-image \"llm\" system=\"x86_64-linux\" {\n    os {\n        state-version \"25.05\"\n    }\n    raw-nix {\n        \"services.ollama.enable = true;\"\n    }\n}\n",
    )
    .unwrap();
    fs::write(
        root.join("hosts/web.kdl"),
        "host \"web\" {\n    system \"x86_64-linux\"\n    nixpkgs release=\"25.05\"\n}\n",
    )
    .unwrap();

    let formatter = identity_formatter();
    let tool: semver::Version = "0.3.1".parse().unwrap();

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
    lock.oracle.nixpkgs_rev = "lxcrev12345".into();
    fs::write(root.join("knixl.lock.kdl"), lock.render()).unwrap();

    let project = gather(&root, &formatter, tool).expect("gather");

    // The guest-image module: modulesPath formals + the lxc-container base + the module tree
    // (and a raw-nix seam), NOT re-rooted (unlike an nspawn guest).
    let module = project
        .generated
        .get(&PathBuf::from("generated/guest-image/llm.nix"))
        .unwrap_or_else(|| {
            panic!(
                "generated/guest-image/llm.nix missing: {:?}",
                project.generated.keys().collect::<Vec<_>>()
            )
        });
    assert!(module.contains("modulesPath"), "formals: {module}");
    assert!(
        module.contains("(modulesPath + \"/virtualisation/lxc-container.nix\")"),
        "lxc-container base import: {module}"
    );
    assert!(
        module.contains("system.stateVersion = \"25.05\""),
        "module tree lowered at top level: {module}"
    );
    assert!(
        module.contains("services.ollama.enable = true;"),
        "raw-nix seam: {module}"
    );

    // The flake: rootfs + metadata outputs, pinned to the oracle rev, no ISO.
    let flake = project
        .generated
        .get(&PathBuf::from("generated/flake.nix"))
        .expect("flake present");
    assert!(
        flake.contains("lxcrev12345"),
        "pinned to oracle rev: {flake}"
    );
    assert!(
        flake.contains("\"llm-lxc\" = image_llm.config.system.build.tarball;"),
        "lxc rootfs output: {flake}"
    );
    assert!(
        flake.contains("\"llm-lxc-metadata\" = image_llm.config.system.build.metadata;"),
        "lxc metadata output: {flake}"
    );
    assert!(
        !flake.contains("-iso"),
        "no ISO output for a guest image: {flake}"
    );

    let _ = fs::remove_dir_all(&root);
}

/// The real formatter, honouring `KNIXL_FORMATTER` like the golden tests. `cat` cannot tell
/// valid Nix from invalid, so only this catches an emitted module that does not parse.
fn real_formatter() -> Formatter {
    let bin = std::env::var("KNIXL_FORMATTER").unwrap_or_else(|_| "nixfmt-rfc-style".into());
    Formatter::detect("nixfmt-rfc-style", PathBuf::from(bin), "0.6.0")
}

#[test]
fn image_target_modules_parse_under_the_real_formatter() {
    // Both image kinds shipped emitting invalid Nix (#81): every test formatted with `cat`, so
    // nothing ever parsed the output. gather formats what it emits, so a syntax error in an
    // image-target module is a gather failure here.
    let formatter = real_formatter();
    if formatter.format("{ }\n").is_err() {
        eprintln!("skipping image_target_modules_parse_under_the_real_formatter: no formatter (set KNIXL_FORMATTER)");
        return;
    }

    let root = temp_root("image-parse");
    fs::write(
        root.join("knixl.kdl"),
        "installer \"usb\" system=\"x86_64-linux\" {\n    os {\n        state-version \"25.05\"\n    }\n}\nguest-image \"llm\" system=\"x86_64-linux\" {\n    os {\n        state-version \"25.05\"\n    }\n}\n",
    )
    .unwrap();
    fs::write(
        root.join("hosts/web.kdl"),
        "host \"web\" {\n    system \"x86_64-linux\"\n}\n",
    )
    .unwrap();

    let project = gather(&root, &formatter, "0.3.1".parse().unwrap())
        .expect("gather: an image-target module failed to format, so it is not valid Nix");

    for name in [
        "generated/installer/usb.nix",
        "generated/guest-image/llm.nix",
    ] {
        assert!(
            project.generated.contains_key(&PathBuf::from(name)),
            "{name} missing: {:?}",
            project.generated.keys().collect::<Vec<_>>()
        );
    }

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

const NIXPKGS_REV: &str = "241313f4e8e508cb9b13278c2b0fa25b9ca27163";
const DISKO_REV: &str = "ff8702b4de27f72b4c78573dfb89ec74e36abdf1";

/// An input-mode project (ADR 0014): two inputs, one host pinned to an exact nixpkgs commit.
fn input_mode_root(tag: &str) -> PathBuf {
    let root = temp_root(tag);
    fs::write(
        root.join("knixl.kdl"),
        "system {\n    state-version \"25.11\"\n    formatter \"nixfmt-rfc-style\"\n    input \"nixpkgs\" url=\"github:NixOS/nixpkgs\"\n    input \"disko\" url=\"github:nix-community/disko\" {\n        follows nixpkgs=\"nixpkgs\"\n    }\n}\n",
    )
    .unwrap();
    fs::write(
        root.join("hosts/type40.kdl"),
        format!("host \"type40\" {{\n    system \"x86_64-linux\"\n    nixpkgs release=\"unstable\" rev=\"{NIXPKGS_REV}\"\n}}\n"),
    )
    .unwrap();
    root
}

fn seed_lock(root: &std::path::Path, flake_inputs: Vec<knixl_lock::model::FlakeInputPin>) {
    let tool: semver::Version = "0.3.1".parse().unwrap();
    let mut lock = gather(root, &identity_formatter(), tool)
        .expect("gather (seed)")
        .lock;
    lock.baselines.insert(
        "type40".to_string(),
        HostBaseline {
            release: "unstable".into(),
            nixpkgs_rev: NIXPKGS_REV.into(),
            options_hash: String::new(),
            modules: Vec::new(),
        },
    );
    lock.flake_inputs = flake_inputs;
    fs::write(root.join("knixl.lock.kdl"), lock.render()).unwrap();
}

fn disko_pin() -> knixl_lock::model::FlakeInputPin {
    knixl_lock::model::FlakeInputPin {
        name: "disko".into(),
        url: "github:nix-community/disko".into(),
        rev: DISKO_REV.into(),
    }
}

#[test]
fn input_mode_emits_pinned_inputs_and_nixos_system_hosts() {
    let root = input_mode_root("input-mode");
    seed_lock(&root, vec![disko_pin()]);

    let project = gather(&root, &identity_formatter(), "0.3.1".parse().unwrap()).expect("gather");
    assert!(
        project.inputs.validation_errors.is_empty(),
        "{:?}",
        project.inputs.validation_errors
    );
    let flake = &project.generated[&PathBuf::from("generated/flake.nix")];
    assert!(
        flake.contains(&format!("url = \"github:NixOS/nixpkgs/{NIXPKGS_REV}\";")),
        "{flake}"
    );
    assert!(
        flake.contains(&format!(
            "url = \"github:nix-community/disko/{DISKO_REV}\";"
        )),
        "{flake}"
    );
    assert!(
        flake.contains("inputs.\"nixpkgs\".follows = \"nixpkgs\";"),
        "{flake}"
    );
    assert!(
        flake.contains(
            "\"type40\" = nixpkgs.lib.nixosSystem {\n          system = \"x86_64-linux\";"
        ),
        "{flake}"
    );
    assert!(
        flake.contains("nixpkgs.legacyPackages.\"x86_64-linux\".\"nixfmt-rfc-style\""),
        "{flake}"
    );

    // No flake.lock yet: that is a `check` problem, never a generate refusal.
    assert!(project.flake_lock_problems[0].contains("is missing"));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn input_mode_checks_flake_lock_against_the_pinned_revs() {
    let root = input_mode_root("input-lock");
    seed_lock(&root, vec![disko_pin()]);
    fs::create_dir_all(root.join("generated")).unwrap();
    let flake_lock = |nixpkgs: &str| {
        format!(
            r#"{{"nodes":{{"disko":{{"locked":{{"rev":"{DISKO_REV}"}}}},"nixpkgs":{{"locked":{{"rev":"{nixpkgs}"}}}},"root":{{"inputs":{{"disko":"disko","nixpkgs":"nixpkgs"}}}}}},"root":"root","version":7}}"#
        )
    };
    fs::write(root.join("generated/flake.lock"), flake_lock(NIXPKGS_REV)).unwrap();
    let tool: semver::Version = "0.3.1".parse().unwrap();
    let project = gather(&root, &identity_formatter(), tool.clone()).expect("gather");
    assert!(
        project.flake_lock_problems.is_empty(),
        "{:?}",
        project.flake_lock_problems
    );

    fs::write(
        root.join("generated/flake.lock"),
        flake_lock(&"0".repeat(40)),
    )
    .unwrap();
    let project = gather(&root, &identity_formatter(), tool).expect("gather");
    assert_eq!(
        project.flake_lock_problems.len(),
        1,
        "{:?}",
        project.flake_lock_problems
    );
    assert!(project.flake_lock_problems[0].contains("pins `nixpkgs`"));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn input_mode_refuses_an_unresolved_input_and_emits_no_flake() {
    let root = input_mode_root("input-unresolved");
    seed_lock(&root, vec![]);

    let project = gather(&root, &identity_formatter(), "0.3.1".parse().unwrap()).expect("gather");
    assert!(
        project
            .inputs
            .validation_errors
            .iter()
            .any(|e| e == "flake input \"disko\" is not resolved: run knixl upgrade"),
        "{:?}",
        project.inputs.validation_errors
    );
    assert!(!project
        .generated
        .contains_key(&PathBuf::from("generated/flake.nix")));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn input_mode_refuses_hosts_on_different_baselines() {
    let root = input_mode_root("input-split");
    fs::write(
        root.join("hosts/other.kdl"),
        "host \"other\" {\n    system \"x86_64-linux\"\n    nixpkgs release=\"25.05\"\n}\n",
    )
    .unwrap();
    seed_lock(&root, vec![disko_pin()]);
    let mut lock =
        knixl_lock::Lock::parse(&fs::read_to_string(root.join("knixl.lock.kdl")).unwrap()).unwrap();
    lock.baselines.insert(
        "other".to_string(),
        HostBaseline {
            release: "25.05".into(),
            nixpkgs_rev: "1".repeat(40),
            options_hash: String::new(),
            modules: Vec::new(),
        },
    );
    fs::write(root.join("knixl.lock.kdl"), lock.render()).unwrap();

    let project = gather(&root, &identity_formatter(), "0.3.1".parse().unwrap()).expect("gather");
    assert!(
        project
            .inputs
            .validation_errors
            .iter()
            .any(|e| e.contains("needs every host on one nixpkgs baseline rev")),
        "{:?}",
        project.inputs.validation_errors
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_host_rev_pin_differing_from_the_lock_is_unresolved() {
    let root = input_mode_root("rev-moved");
    seed_lock(&root, vec![disko_pin()]);
    fs::write(
        root.join("hosts/type40.kdl"),
        format!("host \"type40\" {{\n    system \"x86_64-linux\"\n    nixpkgs release=\"unstable\" rev=\"{}\"\n}}\n", "2".repeat(40)),
    )
    .unwrap();

    let project = gather(&root, &identity_formatter(), "0.3.1".parse().unwrap()).expect("gather");
    assert!(
        project.inputs.validation_errors.iter().any(|e| e
            .starts_with("host \"type40\": nixpkgs release \"unstable\" at 2222")
            && e.ends_with("is not resolved: run knixl upgrade")),
        "{:?}",
        project.inputs.validation_errors
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn input_modules_are_imported_by_hosts_only() {
    let root = input_mode_root("input-modules");
    let kdl = fs::read_to_string(root.join("knixl.kdl")).unwrap();
    fs::write(
        root.join("knixl.kdl"),
        format!("{kdl}oracle-modules {{\n    module \"disko\" input=\"disko\" attr=\"disko\"\n}}\ninstaller \"usb\" system=\"x86_64-linux\" {{\n    os {{\n        state-version \"25.11\"\n    }}\n}}\n"),
    )
    .unwrap();
    seed_lock(&root, vec![disko_pin()]);

    let project = gather(&root, &identity_formatter(), "0.3.1".parse().unwrap()).expect("gather");
    assert!(
        project.inputs.validation_errors.is_empty(),
        "{:?}",
        project.inputs.validation_errors
    );
    let flake = &project.generated[&PathBuf::from("generated/flake.nix")];
    assert!(
        flake.contains("modules = [\n            inputs.\"disko\".nixosModules.\"disko\"\n            ./hosts/type40.nix\n"),
        "{flake}"
    );
    assert!(
        flake.contains("modules = [\n          ./installer/usb.nix\n"),
        "the installer does not import input modules: {flake}"
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn an_input_module_naming_an_undeclared_input_is_refused() {
    let root = input_mode_root("input-module-undeclared");
    let kdl = fs::read_to_string(root.join("knixl.kdl")).unwrap();
    fs::write(
        root.join("knixl.kdl"),
        format!("{kdl}oracle-modules {{\n    module \"sops\" input=\"sops-nix\" attr=\"sops\"\n    module \"np\" input=\"nixpkgs\"\n    module \"both\" input=\"disko\" flake=\"github:x/y\"\n}}\n"),
    )
    .unwrap();
    seed_lock(&root, vec![disko_pin()]);

    let project = gather(&root, &identity_formatter(), "0.3.1".parse().unwrap()).expect("gather");
    let errs = &project.inputs.validation_errors;
    for needle in [
        "oracle module \"sops\" names input `sops-nix`, which is not declared",
        "oracle module \"np\" names input `nixpkgs`",
        "oracle module \"both\" declares both flake= and input=",
    ] {
        assert!(
            errs.iter().any(|e| e.starts_with(needle)),
            "{needle}: {errs:?}"
        );
    }

    let _ = fs::remove_dir_all(&root);
}
