//! Golden tests: the behaviour contract from `examples/`.
//!
//! Each `hosts/*.kdl` input, run through `generate`, must reproduce the corresponding
//! `expected/*.nix` byte-for-byte (post-nixfmt), and the produced lock must match
//! `knixl.lock.kdl`. These are `#[ignore]`d for now because the pipeline stages are
//! still elided (see the crate docs): the whole point is that they turn green as Phase 1
//! lands emit, determinism, and the formatter. Run them with `cargo test -- --ignored`
//! to watch progress.

use std::fs;
use std::path::{Path, PathBuf};

use knixl_lock::Lock;
use knixl_modules::builtin::register_builtins;
use knixl_modules::template::DeclarativeModule;
use knixl_modules::Registry;
use knixl_nix::Formatter;
use knixl_pipeline::{generate, HostSource};

fn examples_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples")
}

fn modules_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../modules")
}

/// Registry as the CLI would build it: built-ins first, then every declarative module
/// found under `modules/<name>/knixl-module.kdl`.
fn build_registry() -> Registry {
    let mut reg = Registry::new();
    register_builtins(&mut reg);

    let modules = modules_dir();
    if let Ok(entries) = fs::read_dir(&modules) {
        for entry in entries.flatten() {
            let manifest = entry.path().join("knixl-module.kdl");
            if !manifest.exists() {
                continue;
            }
            let src = fs::read_to_string(&manifest).expect("read module manifest");
            let doc = knixl_kdl::parse(&src).expect("parse module manifest");
            let module =
                DeclarativeModule::from_kdl(&doc, &manifest).expect("load declarative module");
            reg.register(Box::new(module))
                .expect("register declarative module");
        }
    }
    reg
}

/// The real formatter, honouring `KNIXL_FORMATTER` (point it at a nixfmt wrapper). Records
/// the binary's actual version.
fn formatter() -> Formatter {
    let bin = std::env::var("KNIXL_FORMATTER").unwrap_or_else(|_| "nixfmt-rfc-style".into());
    Formatter::detect("nixfmt-rfc-style", PathBuf::from(bin), "0.6.0")
}

/// An identity "formatter" (`cat`), so the full pipeline can be exercised end to end even
/// where nixfmt is not installed. The text is the emitter's structural output, pre-format.
fn identity_formatter() -> Formatter {
    Formatter {
        name: "identity".into(),
        version: "0".into(),
        bin: PathBuf::from("cat"),
    }
}

fn generate_host(host_file: &str) -> Vec<knixl_pipeline::GeneratedFile> {
    let examples = examples_dir();
    let path = PathBuf::from("hosts").join(host_file);
    let src = fs::read_to_string(examples.join(&path)).expect("read host kdl");
    let tool = "0.3.1".parse().unwrap();
    let no_pins = std::collections::BTreeMap::new();
    let no_oracles = std::collections::BTreeMap::new();
    generate(
        &[HostSource { path, src }],
        &build_registry(),
        &identity_formatter(),
        &tool,
        &no_oracles,
        &no_pins,
        knixl_modules::SecretsBackend::default(),
    )
    .expect("generate")
}

/// Assemble a realistic project root in a temp dir: hosts + lock from examples/, and the
/// module library from the repo's modules/. Returns the root.
fn temp_project(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("knixl-proj-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("hosts")).unwrap();
    fs::create_dir_all(root.join("modules/web-service")).unwrap();

    let examples = examples_dir();
    for host in ["web.kdl", "db.kdl"] {
        fs::copy(
            examples.join("hosts").join(host),
            root.join("hosts").join(host),
        )
        .unwrap();
    }
    fs::copy(examples.join("knixl.lock.kdl"), root.join("knixl.lock.kdl")).unwrap();
    fs::copy(
        examples.join("../modules/web-service/knixl-module.kdl"),
        root.join("modules/web-service/knixl-module.kdl"),
    )
    .unwrap();
    root
}

#[test]
fn gather_and_plan_report_missing_when_disk_is_empty() {
    use knixl_lock::{FileState, Plan};
    use knixl_pipeline::gather::gather;

    let root = temp_project("missing");
    let project = gather(&root, &identity_formatter(), "0.3.1".parse().unwrap()).expect("gather");
    // The project has hosts + modules + a lock, but no generated/ dir, so nothing is on disk.
    let plan = Plan::compute(
        &project.inputs,
        &project.disk,
        &project.lock,
        &project.versions,
    );

    assert!(!plan.has_validation_errors());
    assert_eq!(plan.files.len(), 3, "web.nix, db.nix, db-backup.nix");
    assert!(
        plan.files
            .iter()
            .all(|f| matches!(f.state, FileState::Missing { .. })),
        "every output is Missing when nothing is generated on disk"
    );
    // the declarative module was discovered and registered alongside the built-ins
    assert!(project.registry.get("web-service").is_some());
    assert!(project.registry.get("postgres").is_some());

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn unknown_child_node_surfaces_as_a_warning_not_an_error() {
    // `host` has open_children, so an unclaimed child passes schema validation and is
    // linted during lowering. That lint must reach the generated file as a warning
    // rather than being silently dropped.
    let src = "host \"web\" {\n    system \"x86_64-linux\"\n    mystery-service\n}".to_string();
    let tool = "0.3.1".parse().unwrap();
    let no_pins = std::collections::BTreeMap::new();
    let no_oracles = std::collections::BTreeMap::new();
    let files = generate(
        &[HostSource {
            path: PathBuf::from("hosts/web.kdl"),
            src,
        }],
        &build_registry(),
        &identity_formatter(),
        &tool,
        &no_oracles,
        &no_pins,
        knixl_modules::SecretsBackend::default(),
    )
    .expect("generate");

    assert_eq!(files.len(), 1);
    assert!(
        files[0]
            .warnings
            .iter()
            .any(|w| w.contains("mystery-service")),
        "unknown child should surface as a warning, got {:?}",
        files[0].warnings
    );
}

#[test]
fn web_pipeline_produces_expected_structure() {
    let files = generate_host("web.kdl");
    assert_eq!(files.len(), 1, "web has no side-files");
    let text = &files[0].text;
    for needle in [
        "nixpkgs.hostPlatform = \"x86_64-linux\"",
        "services.nginx.enable = true",
        "services.nginx.virtualHosts.\"example.com\".forceSSL = true",
        "services.nginx.virtualHosts.\"example.com\".locations.\"/\".proxyPass = \"http://127.0.0.1:3000\"",
        "security.acme.certs.\"example.com\".email = \"ops@example.com\"",
        // raw-nix passthrough
        "systemd.services.nginx.serviceConfig.MemoryMax = \"512M\"",
    ] {
        assert!(text.contains(needle), "web.nix missing `{needle}`\n---\n{text}");
    }
}

#[test]
fn db_pipeline_produces_two_files_with_mkif_backup() {
    let files = generate_host("db.kdl");
    assert_eq!(files.len(), 2, "db has a backup side-file");

    let db = files
        .iter()
        .find(|f| f.path.file_name().unwrap() == "db.nix")
        .expect("db.nix");
    assert!(db.text.contains("services.postgresql.enable = true"));
    assert!(
        db.text.contains("lib.mkForce"),
        "listen-tcp forces the preset"
    );
    assert!(
        db.text.contains("./db-backup.nix"),
        "main file imports the side-file"
    );

    let backup = files
        .iter()
        .find(|f| f.path.file_name().unwrap() == "db-backup.nix")
        .expect("db-backup.nix");
    // Pre-format the dynamic host key is quoted (services.restic.backups."db").
    assert!(backup.text.contains("services.restic.backups"));
    assert!(
        backup.text.contains("lib.mkIf"),
        "backup is gated by a runtime condition"
    );
}

#[test]
fn lock_records_only_the_modules_that_contributed_to_each_file() {
    let files = generate_host("db.kdl");
    let db = files
        .iter()
        .find(|f| f.path.file_name().unwrap() == "db.nix")
        .expect("db.nix");
    let backup = files
        .iter()
        .find(|f| f.path.file_name().unwrap() == "db-backup.nix")
        .expect("db-backup.nix");

    assert!(
        db.modules.contains(&"host".to_string()),
        "db.nix modules: {:?}",
        db.modules
    );
    assert!(
        db.modules.contains(&"postgres".to_string()),
        "db.nix modules: {:?}",
        db.modules
    );
    assert!(
        !db.modules.contains(&"backups".to_string()),
        "backups belongs to the side-file, not db.nix: {:?}",
        db.modules
    );
    assert_eq!(
        backup.modules,
        vec!["backups".to_string()],
        "db-backup.nix is backups only"
    );
}

#[test]
fn web_file_attributes_every_contributing_module() {
    let files = generate_host("web.kdl");
    let web = &files[0];
    for m in ["host", "web-service", "raw-nix"] {
        assert!(
            web.modules.contains(&m.to_string()),
            "web.nix should list {m}, got {:?}",
            web.modules
        );
    }
}

#[test]
fn nas_pipeline_produces_expected_structure() {
    let files = generate_host("nas.kdl");
    assert_eq!(files.len(), 1, "nas has no side-files");
    let text = &files[0].text;
    for needle in [
        "networking.hostId = \"8425e349\"",
        "boot.supportedFilesystems.zfs = true",
        "boot.zfs.extraPools",
        "services.zfs.autoScrub.enable = true",
        "options zfs zfs_arc_max=8589934592",
        "users.users.\"wes\".isNormalUser = true",
        "users.users.\"wes\".description = \"Wes Mason\"",
        "users.users.\"wes\".openssh.authorizedKeys.keys",
        "services.openssh.settings.PasswordAuthentication = false",
        "services.openssh.settings.KbdInteractiveAuthentication = false",
        "services.openssh.ports",
        "services.openssh.settings.PermitRootLogin = \"prohibit-password\"",
    ] {
        assert!(
            text.contains(needle),
            "nas.nix missing `{needle}`\n---\n{text}"
        );
    }
    // openssh has no port omitted here, but the empty-collect-opt promise is unit-tested
    // in knixl-modules; here we assert the ports line IS present because ports were given.
}

#[test]
fn nas_file_attributes_every_contributing_module() {
    let files = generate_host("nas.kdl");
    let nas = &files[0];
    for m in ["host", "zfs", "user", "openssh"] {
        assert!(
            nas.modules.contains(&m.to_string()),
            "nas.nix should list {m}, got {:?}",
            nas.modules
        );
    }
}

#[test]
fn vault_pipeline_produces_expected_structure() {
    let files = generate_host("vault.kdl");
    assert_eq!(files.len(), 1, "vault has no side-files");
    let text = &files[0].text;
    // Distinguishing leaf fragments; the byte-exact form is nailed by vault_matches_golden.
    for needle in [
        "/dev/nvme0n1",
        "\"disk\"",
        "\"gpt\"",
        "\"ESP\"",
        "\"crypt\"",
        "\"data\"",
        "\"swap\"",
        "\"EF00\"",
        "\"filesystem\"",
        "\"vfat\"",
        "\"luks\"",
        "\"cryptroot\"",
        "\"zfs\"",
        "pool = \"tank\"",
        "\"zpool\"",
        "datasets",
        "\"zfs_fs\"",
    ] {
        assert!(
            text.contains(needle),
            "vault.nix missing `{needle}`\n---\n{text}"
        );
    }
}

#[test]
fn vault_file_attributes_disko() {
    let files = generate_host("vault.kdl");
    let vault = &files[0];
    for m in ["host", "disko"] {
        assert!(
            vault.modules.contains(&m.to_string()),
            "vault.nix should list {m}, got {:?}",
            vault.modules
        );
    }
}

#[test]
fn gateway_pipeline_produces_expected_structure() {
    let files = generate_host("gateway.kdl");
    assert_eq!(files.len(), 1, "gateway has no side-files");
    let text = &files[0].text;
    for needle in [
        "services.tailscale.enable = true",
        "services.tailscale.extraUpFlags",
        "\"--ssh\"",
        "services.tailscale.authKeyFile = config.sops.secrets.\"tailscale-authkey\".path",
    ] {
        assert!(
            text.contains(needle),
            "gateway.nix missing `{needle}`\n---\n{text}"
        );
    }
}

#[test]
fn gateway_file_attributes_tailscale() {
    let files = generate_host("gateway.kdl");
    let gw = &files[0];
    for m in ["host", "tailscale"] {
        assert!(
            gw.modules.contains(&m.to_string()),
            "gateway.nix should list {m}, got {:?}",
            gw.modules
        );
    }
}

#[test]
fn gateway_agenix_backend_emits_age_path() {
    // The project-level backend flows generate -> LowerCtx -> the (secret) form.
    let examples = examples_dir();
    let path = PathBuf::from("hosts").join("gateway.kdl");
    let src = fs::read_to_string(examples.join(&path)).expect("read host kdl");
    let tool = "0.3.1".parse().unwrap();
    let no_pins = std::collections::BTreeMap::new();
    let no_oracles = std::collections::BTreeMap::new();
    let files = generate(
        &[HostSource { path, src }],
        &build_registry(),
        &identity_formatter(),
        &tool,
        &no_oracles,
        &no_pins,
        knixl_modules::SecretsBackend::Agenix,
    )
    .expect("generate");
    assert!(
        files[0].text.contains(
            "services.tailscale.authKeyFile = config.age.secrets.\"tailscale-authkey\".path"
        ),
        "agenix backend should emit an age path\n---\n{}",
        files[0].text
    );
}

#[test]
fn tailscale_without_auth_key_emits_no_auth_key_file() {
    // No `auth-key` child at all => the for-each has nothing to iterate, so
    // authKeyFile must not appear (as opposed to being emitted empty).
    let path = PathBuf::from("hosts").join("gateway-no-authkey.kdl");
    let src = "host \"gateway\" {\n\
        \x20   system \"x86_64-linux\"\n\
        \x20   tailscale {\n\
        \x20       up-flag \"--ssh\"\n\
        \x20   }\n\
        }"
    .to_string();
    let tool = "0.3.1".parse().unwrap();
    let no_pins = std::collections::BTreeMap::new();
    let no_oracles = std::collections::BTreeMap::new();
    let files = generate(
        &[HostSource { path, src }],
        &build_registry(),
        &identity_formatter(),
        &tool,
        &no_oracles,
        &no_pins,
        knixl_modules::SecretsBackend::default(),
    )
    .expect("generate");
    let text = &files[0].text;
    assert!(
        !text.contains("authKeyFile"),
        "no auth-key means no authKeyFile:\n---\n{text}"
    );
    assert!(
        text.contains("services.tailscale.enable = true"),
        "tailscale is still enabled:\n---\n{text}"
    );
}

#[test]
fn repeated_block_is_hoisted_into_a_let() {
    // shared.kdl applies the same security-headers block to two vhosts, so the block
    // is bound once and referenced twice (structure visible pre-nixfmt).
    let files = generate_host("shared.kdl");
    assert_eq!(files.len(), 1);
    let text = &files[0].text;
    assert!(text.contains("let"), "a let block is emitted:\n{text}");
    assert!(
        text.contains("_knixl0 ="),
        "the shared block is bound:\n{text}"
    );
    let refs = text.matches("= _knixl0;").count();
    assert_eq!(refs, 2, "both vhosts reference the binding:\n{text}");
}

/// True if the configured formatter actually runs. The byte-for-byte goldens need a real
/// nixfmt (set `KNIXL_FORMATTER` to one, e.g. a wrapper); without it they skip rather than
/// fail, so `cargo test` is green on hosts without nixfmt.
fn formatter_available() -> bool {
    formatter().format("{ }\n").is_ok()
}

/// Generate `host_file` and assert every produced file matches `expected/<basename>`.
fn assert_host_matches(host_file: &str) {
    let examples = examples_dir();
    let path = PathBuf::from("hosts").join(host_file);
    let src = fs::read_to_string(examples.join(&path)).expect("read host kdl");

    let registry = build_registry();
    let tool = "0.3.1".parse().unwrap();
    let no_pins = std::collections::BTreeMap::new();
    let no_oracles = std::collections::BTreeMap::new();
    let files = generate(
        &[HostSource { path, src }],
        &registry,
        &formatter(),
        &tool,
        &no_oracles,
        &no_pins,
        knixl_modules::SecretsBackend::default(),
    )
    .expect("generate");

    assert!(
        !files.is_empty(),
        "generate produced no files for {host_file}"
    );
    for f in files {
        let basename = f.path.file_name().expect("output has a file name");
        let expected_path = examples.join("expected").join(basename);
        let expected = fs::read_to_string(&expected_path)
            .unwrap_or_else(|_| panic!("no expected output at {}", expected_path.display()));
        assert_eq!(
            f.text,
            expected,
            "generated {} does not match golden",
            f.path.display()
        );
    }
}

#[test]
fn web_matches_golden() {
    if !formatter_available() {
        eprintln!("skipping web_matches_golden: no formatter (set KNIXL_FORMATTER)");
        return;
    }
    assert_host_matches("web.kdl");
}

#[test]
fn db_matches_golden() {
    if !formatter_available() {
        eprintln!("skipping db_matches_golden: no formatter (set KNIXL_FORMATTER)");
        return;
    }
    assert_host_matches("db.kdl");
}

#[test]
fn nas_matches_golden() {
    if !formatter_available() {
        eprintln!("skipping nas_matches_golden: no formatter (set KNIXL_FORMATTER)");
        return;
    }
    assert_host_matches("nas.kdl");
}

#[test]
fn vault_matches_golden() {
    if !formatter_available() {
        eprintln!("skipping vault_matches_golden: no formatter (set KNIXL_FORMATTER)");
        return;
    }
    assert_host_matches("vault.kdl");
}

#[test]
fn shared_matches_golden() {
    // Exercises let-hoisting through the full pipeline and the pinned nixfmt: the shared
    // security-headers block is bound once and referenced at both vhosts.
    if !formatter_available() {
        eprintln!("skipping shared_matches_golden: no formatter (set KNIXL_FORMATTER)");
        return;
    }
    assert_host_matches("shared.kdl");
}

#[test]
fn gateway_matches_golden() {
    if !formatter_available() {
        eprintln!("skipping gateway_matches_golden: no formatter (set KNIXL_FORMATTER)");
        return;
    }
    assert_host_matches("gateway.kdl");
}

#[test]
fn generate_is_byte_identical_across_runs() {
    if !formatter_available() {
        eprintln!("skipping determinism golden: no formatter (set KNIXL_FORMATTER)");
        return;
    }
    let examples = examples_dir();
    let path = PathBuf::from("hosts/web.kdl");
    let src = fs::read_to_string(examples.join(&path)).expect("read host kdl");
    let tool = "0.3.1".parse().unwrap();
    let no_pins = std::collections::BTreeMap::new();
    let no_oracles = std::collections::BTreeMap::new();

    let run = || {
        generate(
            &[HostSource {
                path: path.clone(),
                src: src.clone(),
            }],
            &build_registry(),
            &formatter(),
            &tool,
            &no_oracles,
            &no_pins,
            knixl_modules::SecretsBackend::default(),
        )
        .expect("generate")
        .into_iter()
        .map(|f| (f.path, f.text))
        .collect::<Vec<_>>()
    };

    assert_eq!(run(), run(), "two generate runs produced different bytes");
}

#[test]
fn lock_round_trips() {
    let src = fs::read_to_string(examples_dir().join("knixl.lock.kdl")).expect("read lock");
    let lock = Lock::parse(&src).expect("parse example lock");
    // The hand-written example carries a comment header and its own spacing, so it is not
    // byte-identical to render(). Assert a structural round-trip instead: parsing our own
    // render reproduces the same model.
    assert_eq!(Lock::parse(&lock.render()).expect("re-parse"), lock);
}

/// The pinned emit path (issue #25): a host with one version-pinned package (htop, mixed in
/// from a historical nixpkgs commit) and one ambient package (ripgrep). The rev comes from
/// the committed example lock, so generation stays offline and deterministic.
fn pinned_pins() -> std::collections::BTreeMap<String, Vec<knixl_lock::model::Pin>> {
    let lock_src = fs::read_to_string(examples_dir().join("knixl.lock.kdl")).expect("read lock");
    Lock::parse(&lock_src).expect("parse example lock").pins
}

#[test]
fn pinned_matches_golden() {
    if !formatter_available() {
        eprintln!("skipping pinned_matches_golden: no formatter (set KNIXL_FORMATTER)");
        return;
    }
    let examples = examples_dir();
    let path = PathBuf::from("hosts/pinned.kdl");
    let src = fs::read_to_string(examples.join(&path)).expect("read pinned host kdl");
    let pins = pinned_pins();
    let tool = "0.3.1".parse().unwrap();
    let no_oracles = std::collections::BTreeMap::new();

    let files = generate(
        &[HostSource { path, src }],
        &build_registry(),
        &formatter(),
        &tool,
        &no_oracles,
        &pins,
        knixl_modules::SecretsBackend::default(),
    )
    .expect("generate");

    assert_eq!(files.len(), 1, "pinned host has no side-files");
    let expected = fs::read_to_string(examples.join("expected/pinned.nix"))
        .expect("no expected output at examples/expected/pinned.nix");
    assert_eq!(files[0].text, expected, "pinned.nix does not match golden");
}

#[test]
fn pinned_generate_is_byte_identical_across_runs() {
    if !formatter_available() {
        eprintln!("skipping pinned determinism golden: no formatter (set KNIXL_FORMATTER)");
        return;
    }
    let examples = examples_dir();
    let path = PathBuf::from("hosts/pinned.kdl");
    let src = fs::read_to_string(examples.join(&path)).expect("read pinned host kdl");
    let pins = pinned_pins();
    let tool = "0.3.1".parse().unwrap();
    let no_oracles = std::collections::BTreeMap::new();

    let run = || {
        generate(
            &[HostSource {
                path: path.clone(),
                src: src.clone(),
            }],
            &build_registry(),
            &formatter(),
            &tool,
            &no_oracles,
            &pins,
            knixl_modules::SecretsBackend::default(),
        )
        .expect("generate")
        .into_iter()
        .map(|f| (f.path, f.text))
        .collect::<Vec<_>>()
    };

    assert_eq!(
        run(),
        run(),
        "two generate runs produced different bytes for the pinned host"
    );
}

/// The override emit path (issue #23): a host with one version-pinned package (htop) whose
/// lock pin carries `strategy="override"`, so the package is bound to `pkgs.htop.overrideAttrs`
/// against the historical `src`/`version` rather than imported wholesale.
#[test]
fn pinned_override_matches_golden() {
    if !formatter_available() {
        eprintln!("skipping pinned_override_matches_golden: no formatter (set KNIXL_FORMATTER)");
        return;
    }
    let examples = examples_dir();
    let path = PathBuf::from("hosts/pinned-override.kdl");
    let src = fs::read_to_string(examples.join(&path)).expect("read pinned-override host kdl");
    let pins = pinned_pins();
    let tool = "0.3.1".parse().unwrap();
    let no_oracles = std::collections::BTreeMap::new();

    let files = generate(
        &[HostSource { path, src }],
        &build_registry(),
        &formatter(),
        &tool,
        &no_oracles,
        &pins,
        knixl_modules::SecretsBackend::default(),
    )
    .expect("generate");

    assert_eq!(files.len(), 1, "pinned-override host has no side-files");
    let expected = fs::read_to_string(examples.join("expected/pinned-override.nix"))
        .expect("no expected output at examples/expected/pinned-override.nix");
    assert_eq!(
        files[0].text, expected,
        "pinned-override.nix does not match golden"
    );
}

/// GC (issue #24): a pin for a package no longer declared in the host's KDL must not
/// survive into `lock_next`, while a pin whose package is still declared (with a
/// matching version) does.
#[test]
fn generate_prunes_pins_for_packages_no_longer_declared() {
    use knixl_lock::model::{FormatterPin, OraclePin, Pin, PinStrategy};
    use knixl_lock::Plan;
    use knixl_pipeline::gather::gather;

    let root = std::env::temp_dir().join(format!("knixl-proj-pin-gc-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("hosts")).unwrap();
    // htop was dropped from the host's KDL; jq stays, and remains pinned.
    fs::write(
        root.join("hosts/app.kdl"),
        "host \"app\" {\n    system \"x86_64-linux\"\n    package \"jq\" version=\"1.7\"\n}\n",
    )
    .unwrap();

    let rev = "0000000000000000000000000000000000000abc".to_string();
    let mut pins = std::collections::BTreeMap::new();
    pins.insert(
        "app".to_string(),
        vec![
            Pin {
                package: "jq".into(),
                version: "1.7".into(),
                nixpkgs_rev: rev.clone(),
                strategy: PinStrategy::CommitMix,
            },
            Pin {
                package: "htop".into(),
                version: "3.2.1".into(),
                nixpkgs_rev: rev,
                strategy: PinStrategy::CommitMix,
            },
        ],
    );
    let lock = Lock {
        version: 1,
        tool: "0.3.1".parse().unwrap(),
        formatter: FormatterPin {
            name: "identity".into(),
            version: "0".into(),
        },
        oracle: OraclePin {
            nixpkgs_rev: String::new(),
            options_hash: String::new(),
            modules: Vec::new(),
        },
        inputs: std::collections::BTreeMap::new(),
        modules: std::collections::BTreeMap::new(),
        outputs: Vec::new(),
        pins,
        baselines: std::collections::BTreeMap::new(),
    };
    fs::write(root.join("knixl.lock.kdl"), lock.render()).unwrap();

    let project = gather(&root, &identity_formatter(), "0.3.1".parse().unwrap()).expect("gather");
    let plan = Plan::compute(
        &project.inputs,
        &project.disk,
        &project.lock,
        &project.versions,
    );
    assert!(
        !plan.has_validation_errors(),
        "unexpected validation errors: {:?}",
        plan.validation_errors
    );

    let app_pins = plan
        .lock_next
        .pins
        .get("app")
        .expect("app host keeps its surviving pin");
    assert_eq!(
        app_pins.len(),
        1,
        "htop's pin should have been pruned: {app_pins:?}"
    );
    assert_eq!(app_pins[0].package, "jq");

    let _ = fs::remove_dir_all(&root);
}
