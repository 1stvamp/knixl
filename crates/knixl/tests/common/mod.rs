//! Helpers shared by the integration tests: temp projects, running the binary, and shims
//! standing in for nix.
// Each test binary compiles this module and uses only some of it.
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn examples() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples")
}

pub fn temp_project(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("knixl-cli-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("hosts")).unwrap();
    fs::create_dir_all(root.join("modules/web-service")).unwrap();
    let ex = examples();
    for host in ["web.kdl", "db.kdl"] {
        fs::copy(ex.join("hosts").join(host), root.join("hosts").join(host)).unwrap();
    }
    // No lockfile: a fresh project, so the baseline versions match the running tool
    // (the copied examples lock pins nixfmt 1.3.1, which would skew against `cat`).
    fs::copy(
        ex.join("../crates/knixl-modules/stdlib/web-service/knixl-module.kdl"),
        root.join("modules/web-service/knixl-module.kdl"),
    )
    .unwrap();
    root
}

/// Every run here sets `KNIXL_NIX_BUILD=false`: `upgrade` builds missing NixOS option sets
/// (#128), and a test's fake nixpkgs rev must not send that to the network.
pub fn knixl(root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_knixl"))
        .args(args)
        .current_dir(root)
        .env("KNIXL_FORMATTER", "cat")
        .env("KNIXL_NIX_BUILD", "false")
        .output()
        .expect("run knixl")
}

/// A shim mimicking `nix-instantiate`: `--eval` prints a verdict, `--parse` exits 0.
pub fn nix_shim(tag: &str, exists: bool) -> PathBuf {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    let path = std::env::temp_dir().join(format!("knixl-cli-nixshim-{}-{tag}", std::process::id()));
    let verdict = if exists { "true" } else { "false" };
    let script = format!(
        "#!/bin/sh\ncase \"$1\" in\n  --eval) echo \"{verdict}\" ;;\n  --parse) exit 0 ;;\nesac\n"
    );
    let mut f = fs::File::create(&path).unwrap();
    f.write_all(script.as_bytes()).unwrap();
    f.flush().unwrap();
    drop(f); // close before exec, or spawning races with ETXTBSY
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// A shim mimicking `nix-build`: exits 0 when `build_ok`, else 1.
pub fn build_shim(tag: &str, build_ok: bool) -> PathBuf {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    let path =
        std::env::temp_dir().join(format!("knixl-cli-buildshim-{}-{tag}", std::process::id()));
    let exit = if build_ok { 0 } else { 1 };
    let script = format!("#!/bin/sh\nexit {exit}\n");
    let mut f = fs::File::create(&path).unwrap();
    f.write_all(script.as_bytes()).unwrap();
    f.flush().unwrap();
    drop(f); // close before exec, or spawning races with ETXTBSY
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// A `nix-build` shim that records its arguments to `log` and exits 0.
pub fn recording_build_shim(tag: &str, log: &Path) -> PathBuf {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    let path = std::env::temp_dir().join(format!(
        "knixl-cli-recbuildshim-{}-{tag}",
        std::process::id()
    ));
    let script = format!("#!/bin/sh\necho \"$@\" >> '{}'\nexit 0\n", log.display());
    let mut f = fs::File::create(&path).unwrap();
    f.write_all(script.as_bytes()).unwrap();
    f.flush().unwrap();
    drop(f); // close before exec, or spawning races with ETXTBSY
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// `install --build` against `web`, returning what the build shim was asked to build.
pub fn install_build_args(tag: &str, host_allows_unfree: bool) -> String {
    let root = temp_project(tag);
    if host_allows_unfree {
        let dir = root.join("modules/unfree-test");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("knixl-module.kdl"),
            "module name=\"unfree-test\" version=\"1.0.0\" {\n    summary \"allows unfree, for install build tests.\"\n    claims-node \"unfree-test\"\n    schema { }\n    emit {\n        set \"nixpkgs.config.allowUnfree\" #true\n    }\n}\n",
        )
        .unwrap();
        let host = root.join("hosts/web.kdl");
        let src = fs::read_to_string(&host).unwrap();
        let close = src.rfind('}').unwrap();
        fs::write(
            &host,
            format!("{}    unfree-test\n{}", &src[..close], &src[close..]),
        )
        .unwrap();
    }
    let log = std::env::temp_dir().join(format!(
        "knixl-cli-recbuild-{}-{tag}.log",
        std::process::id()
    ));
    let _ = fs::remove_file(&log);
    let eval_shim = nix_shim(tag, true);
    let build = recording_build_shim(tag, &log);
    let out = Command::new(env!("CARGO_BIN_EXE_knixl"))
        .args(["install", "ripgrep", "--host", "web", "--yes", "--build"])
        .current_dir(&root)
        .env("KNIXL_FORMATTER", "cat")
        .env("KNIXL_NIX_BUILD", "false")
        .env("KNIXL_NIX", &eval_shim)
        .env("KNIXL_NIX_BUILD", &build)
        .output()
        .expect("run knixl install");
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let args = fs::read_to_string(&log).unwrap_or_default();
    let _ = fs::remove_file(&log);
    let _ = fs::remove_file(&eval_shim);
    let _ = fs::remove_file(&build);
    let _ = fs::remove_dir_all(&root);
    args
}

/// A shim mimicking `knixl-pin-resolve`: prints `stdout_line` and exits `code`.
pub fn resolver_shim(tag: &str, stdout_line: &str, stderr_line: &str, code: i32) -> PathBuf {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    let path = std::env::temp_dir().join(format!(
        "knixl-cli-resolvershim-{}-{tag}",
        std::process::id()
    ));
    let script = format!(
        "#!/bin/sh\n[ -n \"{o}\" ] && echo \"{o}\"\n[ -n \"{e}\" ] && echo \"{e}\" 1>&2\nexit {code}\n",
        o = stdout_line, e = stderr_line,
    );
    let mut f = fs::File::create(&path).unwrap();
    f.write_all(script.as_bytes()).unwrap();
    f.flush().unwrap();
    drop(f); // close before exec, or spawning races with ETXTBSY
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// A shim mimicking `nix-instantiate`: `--eval` says the package exists, but `--parse` fails.
/// Used to reach `commit_install`'s revert path *after* `write_pin` has already run (the pin
/// gate itself passed; the KDL failed to parse once drafted), to check the pin does not
/// linger.
pub fn nix_shim_parse_fails(tag: &str) -> PathBuf {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    let path = std::env::temp_dir().join(format!(
        "knixl-cli-nixshim-parsefail-{}-{tag}",
        std::process::id()
    ));
    let script =
        "#!/bin/sh\ncase \"$1\" in\n  --eval) echo \"true\" ;;\n  --parse) exit 1 ;;\nesac\n";
    let mut f = fs::File::create(&path).unwrap();
    f.write_all(script.as_bytes()).unwrap();
    f.flush().unwrap();
    drop(f); // close before exec, or spawning races with ETXTBSY
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// A shim mimicking `nix-build` for pin-strategy selection: passes the override candidate,
/// fails the commit-mix candidate. The two expressions are distinguished by `overrideAttrs`,
/// which only `override_test_expr` emits.
pub fn strategy_build_shim(tag: &str) -> PathBuf {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    let path = std::env::temp_dir().join(format!(
        "knixl-cli-strategybuildshim-{}-{tag}",
        std::process::id()
    ));
    let script = "#!/bin/sh\ncase \"$*\" in\n  *overrideAttrs*) exit 0 ;;\n  *) exit 1 ;;\nesac\n";
    let mut f = fs::File::create(&path).unwrap();
    f.write_all(script.as_bytes()).unwrap();
    f.flush().unwrap();
    drop(f); // close before exec, or spawning races with ETXTBSY
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// Declares a `nixpkgs release="25.05"` baseline on the copied `web` host, in place.
pub fn declare_baseline_release(root: &Path, release: &str) {
    let path = root.join("hosts/web.kdl");
    let src = fs::read_to_string(&path).unwrap();
    let edited = src.replacen(
        "system \"x86_64-linux\"",
        &format!("system \"x86_64-linux\"\n    nixpkgs release=\"{release}\""),
        1,
    );
    assert_ne!(
        src, edited,
        "system line not found to anchor the nixpkgs node"
    );
    fs::write(&path, edited).unwrap();
}

/// Writes a project-level `knixl.kdl` declaring one `oracle-modules` entry (#35 phase 3).
pub fn declare_project_oracle_module(root: &Path, name: &str, flake: &str) {
    fs::write(
        root.join("knixl.kdl"),
        format!("oracle-modules {{\n    module \"{name}\" flake=\"{flake}\"\n}}\n"),
    )
    .unwrap();
}

/// Splices an `oracle-modules` block declaring one module onto the named host's own KDL
/// (ADR 0008 host override), anchored the same way `declare_baseline_release` is.
pub fn declare_host_oracle_module(root: &Path, host: &str, name: &str, flake: &str) {
    let path = root.join("hosts").join(format!("{host}.kdl"));
    let src = fs::read_to_string(&path).unwrap();
    let edited = src.replacen(
        "system \"x86_64-linux\"",
        &format!(
            "system \"x86_64-linux\"\n    oracle-modules {{\n        module \"{name}\" flake=\"{flake}\"\n    }}"
        ),
        1,
    );
    assert_ne!(
        src, edited,
        "system line not found to anchor the oracle-modules node"
    );
    fs::write(&path, edited).unwrap();
}

/// A shim mimicking `nix-build` for the `nixosOptionsDoc` build (`build_options_json`): writes
/// a throwaway "store path" directory containing `share/doc/nixos/options.json` with `json`,
/// then a script that prints that directory's path and exits 0 (the exact shape
/// `build_options_json` expects from a real `nix-build --no-out-link -E <expr>`).
pub fn optionsdoc_build_shim(tag: &str, json: &str) -> PathBuf {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    let store = std::env::temp_dir().join(format!(
        "knixl-cli-optionsdoc-store-{}-{tag}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&store);
    fs::create_dir_all(store.join("share/doc/nixos")).unwrap();
    fs::write(store.join("share/doc/nixos/options.json"), json).unwrap();

    let path = std::env::temp_dir().join(format!(
        "knixl-cli-optionsdocshim-{}-{tag}",
        std::process::id()
    ));
    let script = format!("#!/bin/sh\necho \"{}\"\n", store.display());
    let mut f = fs::File::create(&path).unwrap();
    f.write_all(script.as_bytes()).unwrap();
    f.flush().unwrap();
    drop(f); // close before exec, or spawning races with ETXTBSY
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// A minimal declarative module manifest whose `emit` targets an out-of-tree `disko.*` option
/// path: a genuine, oracle-checked consumer of the augmented set (unlike `raw-nix`, which
/// bypasses the oracle entirely).
pub fn write_disko_test_module(root: &Path) {
    let dir = root.join("modules/disko-test");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("knixl-module.kdl"),
        "module name=\"disko-test\" version=\"1.0.0\" {\n    summary \"targets an out-of-tree disko option, for oracle testing.\"\n    claims-node \"disko-test\"\n    schema { }\n    emit {\n        set \"disko.devices.disk.main.device\" \"/dev/sda\"\n    }\n}\n",
    )
    .unwrap();
}
