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
            let module = DeclarativeModule::from_kdl(&doc, &manifest).expect("load declarative module");
            reg.register(Box::new(module)).expect("register declarative module");
        }
    }
    reg
}

/// A formatter handle. `format()` is not invoked until it is implemented, so the binary
/// path only needs to be right once Phase 1's formatter integration lands.
fn formatter() -> Formatter {
    Formatter {
        name: "nixfmt-rfc-style".into(),
        version: "0.6.0".into(),
        bin: PathBuf::from("nixfmt-rfc-style"),
    }
}

/// Generate `host_file` and assert every produced file matches `expected/<basename>`.
fn assert_host_matches(host_file: &str) {
    let examples = examples_dir();
    let path = PathBuf::from("hosts").join(host_file);
    let src = fs::read_to_string(examples.join(&path)).expect("read host kdl");

    let registry = build_registry();
    let tool = "0.3.1".parse().unwrap();
    let files = generate(&[HostSource { path, src }], &registry, &formatter(), &tool)
        .expect("generate");

    assert!(!files.is_empty(), "generate produced no files for {host_file}");
    for f in files {
        let basename = f.path.file_name().expect("output has a file name");
        let expected_path = examples.join("expected").join(basename);
        let expected = fs::read_to_string(&expected_path)
            .unwrap_or_else(|_| panic!("no expected output at {}", expected_path.display()));
        assert_eq!(f.text, expected, "generated {} does not match golden", f.path.display());
    }
}

#[test]
#[ignore = "pipeline stubbed until Phase 1; run with --ignored to drive it green"]
fn web_matches_golden() {
    assert_host_matches("web.kdl");
}

#[test]
#[ignore = "pipeline stubbed until Phase 1; run with --ignored to drive it green"]
fn db_matches_golden() {
    assert_host_matches("db.kdl");
}

#[test]
#[ignore = "pipeline stubbed until Phase 1; run with --ignored to drive it green"]
fn generate_is_byte_identical_across_runs() {
    let examples = examples_dir();
    let path = PathBuf::from("hosts/web.kdl");
    let src = fs::read_to_string(examples.join(&path)).expect("read host kdl");
    let tool = "0.3.1".parse().unwrap();

    let run = || {
        generate(
            &[HostSource { path: path.clone(), src: src.clone() }],
            &build_registry(),
            &formatter(),
            &tool,
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
