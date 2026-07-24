//! The fetched module layer (issue #13): a declared `modules {}` source in `knixl.kdl`
//! resolves through the lock's `module-source` pin, never the network, so `gather` (and thus
//! `generate`) stays offline. These tests seed the cache and lock by hand rather than hitting
//! git, and exercise the four contracts `build_registry` must hold:
//!
//! - a declared source with a matching pin and a verified cache entry registers its node;
//! - a declared source with no pin is a validation error naming the fix;
//! - a cached manifest whose hash no longer matches its pin is a hard error, never a silent
//!   refetch;
//! - a fetched module and a local module claiming the same node: the local one wins, with one
//!   shadow notice.
//!
//! All four tests set `XDG_CACHE_HOME` (`module_cache_path`'s lookup), a process-wide env var,
//! so they share one lock to stay safe under parallel test execution within this binary.

use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use knixl_nix::module_fetch::{hash_module, module_cache_path};
use knixl_nix::Formatter;
use knixl_pipeline::gather::gather;

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn identity_formatter() -> Formatter {
    Formatter {
        name: "identity".into(),
        version: "0".into(),
        bin: PathBuf::from("cat"),
    }
}

fn temp_root(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "knixl-fetched-modules-{}-{tag}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("hosts")).unwrap();
    root
}

fn temp_cache_home(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "knixl-fetched-modules-cache-{}-{tag}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// A minimal, valid `knixl-module.kdl` claiming node `"widget"`. Empty `schema`/`emit`
/// blocks type-check (they carry nothing to dry-check against).
fn widget_manifest() -> String {
    "module name=\"widget\" version=\"1.0.0\" {\n    summary \"Test fetched module.\"\n    claims-node \"widget\"\n\n    schema {\n    }\n\n    emit {\n    }\n}\n".to_string()
}

/// The declared `modules {}` block for a project pointing `name` at an arbitrary flake ref.
fn modules_block(name: &str, flake: &str) -> String {
    format!("modules {{\n    module \"{name}\" flake=\"{flake}\"\n}}\n")
}

#[test]
fn fetched_source_with_a_verified_cache_entry_registers_its_node() {
    let _guard = ENV_LOCK.lock().unwrap();
    let root = temp_root("registers");
    let cache_home = temp_cache_home("registers");
    std::env::set_var("XDG_CACHE_HOME", &cache_home);

    fs::write(
        root.join("knixl.kdl"),
        modules_block("widget", "github:example/widget"),
    )
    .unwrap();

    let text = widget_manifest();
    let url = "https://example.com/widget.git";
    let rev = "abc123";
    let cache_path = module_cache_path(url, rev, "").expect("cache path");
    fs::create_dir_all(cache_path.parent().unwrap()).unwrap();
    fs::write(&cache_path, &text).unwrap();

    let formatter = identity_formatter();
    let tool: semver::Version = "0.3.1".parse().unwrap();
    let seed = gather(&root, &formatter, tool.clone()).expect("gather (seed)");
    let mut lock = seed.lock;
    lock.module_sources = vec![knixl_lock::model::ModuleSourcePin {
        name: "widget".into(),
        url: url.into(),
        rev: rev.into(),
        path: "".into(),
        hash: hash_module(&text),
    }];
    fs::write(root.join("knixl.lock.kdl"), lock.render()).unwrap();

    let project = gather(&root, &formatter, tool).expect("gather");
    assert!(
        project.inputs.validation_errors.is_empty(),
        "got: {:?}",
        project.inputs.validation_errors
    );
    assert!(
        project.registry.get("widget").is_some(),
        "the fetched module must register its claimed node"
    );

    std::env::remove_var("XDG_CACHE_HOME");
    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&cache_home);
}

/// Generate loads from `pin.path`, the locked path, not `source.path`, the declared one: a
/// project whose `knixl.kdl` has moved `path=` since the last `install`/`upgrade` stays
/// reproducible against what was actually resolved, rather than reading a path nobody fetched
/// (or worse, silently reading whatever happens to live at the new path today).
#[test]
fn load_at_generate_uses_the_pins_locked_path_not_the_declared_source_path() {
    let _guard = ENV_LOCK.lock().unwrap();
    let root = temp_root("pin-path");
    let cache_home = temp_cache_home("pin-path");
    std::env::set_var("XDG_CACHE_HOME", &cache_home);

    // The declared source now points at "new-path", but the lock's pin (what was actually
    // resolved and cached) still records "old-path".
    fs::write(
        root.join("knixl.kdl"),
        "modules {\n    module \"widget\" flake=\"github:example/widget\" path=\"new-path\"\n}\n",
    )
    .unwrap();

    let text = widget_manifest();
    let url = "https://example.com/widget.git";
    let rev = "abc123";
    let cache_path = module_cache_path(url, rev, "old-path").expect("cache path");
    fs::create_dir_all(cache_path.parent().unwrap()).unwrap();
    fs::write(&cache_path, &text).unwrap();

    let formatter = identity_formatter();
    let tool: semver::Version = "0.3.1".parse().unwrap();
    let seed = gather(&root, &formatter, tool.clone()).expect("gather (seed)");
    let mut lock = seed.lock;
    lock.module_sources = vec![knixl_lock::model::ModuleSourcePin {
        name: "widget".into(),
        url: url.into(),
        rev: rev.into(),
        path: "old-path".into(),
        hash: hash_module(&text),
    }];
    fs::write(root.join("knixl.lock.kdl"), lock.render()).unwrap();

    let project = gather(&root, &formatter, tool).expect("gather");
    assert!(
        project.inputs.validation_errors.is_empty(),
        "got: {:?}",
        project.inputs.validation_errors
    );
    assert!(
        project.registry.get("widget").is_some(),
        "the fetched module must register from the pin's locked path"
    );

    std::env::remove_var("XDG_CACHE_HOME");
    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&cache_home);
}

#[test]
fn declared_source_with_no_pin_is_a_validation_error_naming_install_or_upgrade() {
    let _guard = ENV_LOCK.lock().unwrap();
    let root = temp_root("no-pin");
    let cache_home = temp_cache_home("no-pin");
    std::env::set_var("XDG_CACHE_HOME", &cache_home);

    fs::write(
        root.join("knixl.kdl"),
        modules_block("widget", "github:example/widget"),
    )
    .unwrap();

    let project = gather(&root, &identity_formatter(), "0.3.1".parse().unwrap()).expect("gather");
    assert!(
        project.inputs.validation_errors.iter().any(|e| {
            e.contains("widget") && e.contains("knixl install") && e.contains("upgrade")
        }),
        "got: {:?}",
        project.inputs.validation_errors
    );

    std::env::remove_var("XDG_CACHE_HOME");
    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&cache_home);
}

#[test]
fn a_cached_manifest_whose_hash_no_longer_matches_its_pin_is_a_hard_error() {
    let _guard = ENV_LOCK.lock().unwrap();
    let root = temp_root("hash-mismatch");
    let cache_home = temp_cache_home("hash-mismatch");
    std::env::set_var("XDG_CACHE_HOME", &cache_home);

    fs::write(
        root.join("knixl.kdl"),
        modules_block("widget", "github:example/widget"),
    )
    .unwrap();

    let text = widget_manifest();
    let url = "https://example.com/widget.git";
    let rev = "abc123";
    let cache_path = module_cache_path(url, rev, "").expect("cache path");
    fs::create_dir_all(cache_path.parent().unwrap()).unwrap();
    fs::write(&cache_path, &text).unwrap();

    let formatter = identity_formatter();
    let tool: semver::Version = "0.3.1".parse().unwrap();
    let seed = gather(&root, &formatter, tool.clone()).expect("gather (seed)");
    let mut lock = seed.lock;
    lock.module_sources = vec![knixl_lock::model::ModuleSourcePin {
        name: "widget".into(),
        url: url.into(),
        rev: rev.into(),
        path: "".into(),
        // Deliberately wrong: the pin no longer matches what is cached, as if the cache had
        // been hand-edited or corrupted after being written by `install`/`upgrade`.
        hash: "blake3:0000000000000000000000000000000000000000000000000000000000000000".into(),
    }];
    fs::write(root.join("knixl.lock.kdl"), lock.render()).unwrap();

    let msg = match gather(&root, &formatter, tool) {
        Ok(_) => panic!("hash mismatch must be a hard error"),
        Err(e) => e.to_string(),
    };
    assert!(
        msg.contains("widget") && msg.contains("hash mismatch"),
        "got: {msg}"
    );

    std::env::remove_var("XDG_CACHE_HOME");
    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&cache_home);
}

#[test]
fn a_local_module_shadows_a_fetched_module_claiming_the_same_node() {
    let _guard = ENV_LOCK.lock().unwrap();
    let root = temp_root("shadow");
    let cache_home = temp_cache_home("shadow");
    std::env::set_var("XDG_CACHE_HOME", &cache_home);

    // Local project module claiming "widget" (higher precedence than fetched).
    fs::create_dir_all(root.join("modules/widget")).unwrap();
    fs::write(
        root.join("modules/widget/knixl-module.kdl"),
        widget_manifest(),
    )
    .unwrap();

    fs::write(
        root.join("knixl.kdl"),
        modules_block("widget", "github:example/widget"),
    )
    .unwrap();

    let text = widget_manifest();
    let url = "https://example.com/widget.git";
    let rev = "abc123";
    let cache_path = module_cache_path(url, rev, "").expect("cache path");
    fs::create_dir_all(cache_path.parent().unwrap()).unwrap();
    fs::write(&cache_path, &text).unwrap();

    let formatter = identity_formatter();
    let tool: semver::Version = "0.3.1".parse().unwrap();
    let seed = gather(&root, &formatter, tool.clone()).expect("gather (seed)");
    let mut lock = seed.lock;
    lock.module_sources = vec![knixl_lock::model::ModuleSourcePin {
        name: "widget".into(),
        url: url.into(),
        rev: rev.into(),
        path: "".into(),
        hash: hash_module(&text),
    }];
    fs::write(root.join("knixl.lock.kdl"), lock.render()).unwrap();

    let project = gather(&root, &formatter, tool).expect("gather");
    assert!(
        project.inputs.validation_errors.is_empty(),
        "got: {:?}",
        project.inputs.validation_errors
    );
    assert!(
        project.registry.get("widget").is_some(),
        "local must still win the node"
    );
    let shadow_notices: Vec<&String> = project
        .warnings
        .iter()
        .filter(|w| w.contains("widget") && w.contains("shadows the fetched one"))
        .collect();
    assert_eq!(
        shadow_notices.len(),
        1,
        "exactly one shadow notice for the fetched module: got {:?}",
        project.warnings
    );

    std::env::remove_var("XDG_CACHE_HOME");
    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&cache_home);
}
