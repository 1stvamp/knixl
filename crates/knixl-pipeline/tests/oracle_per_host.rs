//! Per-host oracle selection (issue #22): `gather` must build one oracle per host, keyed by
//! host name, choosing each host's nixpkgs rev from its lock `baseline` when present and
//! falling back to the lock's project-wide rev otherwise. This is its own test binary (a
//! separate file under tests/) so the `XDG_CACHE_HOME` env var it sets does not race other
//! test binaries' cache lookups.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use knixl_ir::{AttrKey, AttrPath, NixExpr};
use knixl_lock::model::{FormatterPin, HostBaseline, OraclePin};
use knixl_lock::Lock;
use knixl_nix::Formatter;
use knixl_pipeline::gather::gather;

fn ident_path(segs: &[&str]) -> AttrPath {
    AttrPath(
        segs.iter()
            .map(|s| AttrKey::Ident((*s).to_string()))
            .collect(),
    )
}

fn identity_formatter() -> Formatter {
    Formatter {
        name: "identity".into(),
        version: "0".into(),
        bin: PathBuf::from("cat"),
    }
}

/// Write a minimal options.json fixture (one known boolean option) into the cache directory
/// `from_rev_cache` reads, keyed by `rev`.
fn seed_cache(rev: &str, known_option: &str) {
    let dest = knixl_oracle::cache_path(rev).expect("cache path under XDG_CACHE_HOME");
    fs::create_dir_all(dest.parent().unwrap()).unwrap();
    fs::write(
        &dest,
        format!(r#"{{ "{known_option}": {{ "type": "boolean" }} }}"#),
    )
    .unwrap();
}

#[test]
fn gather_selects_each_hosts_own_baseline_rev_for_its_oracle() {
    let tmp = std::env::temp_dir().join(format!(
        "knixl-oracle-per-host-cache-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&tmp);
    std::env::set_var("XDG_CACHE_HOME", &tmp);

    // Host "a" has a lock baseline pinned to rev-a, whose cached option set knows only
    // services.foo.enable. Host "b" has no baseline, so it falls back to the lock's
    // project-wide rev-b, whose cached option set knows only services.bar.enable. Distinct
    // known options per rev let the test tell the two oracles apart.
    seed_cache("rev-a", "services.foo.enable");
    seed_cache("rev-b", "services.bar.enable");

    let root =
        std::env::temp_dir().join(format!("knixl-proj-oracle-per-host-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("hosts")).unwrap();
    fs::write(
        root.join("hosts/a.kdl"),
        "host \"a\" {\n    system \"x86_64-linux\"\n}\n",
    )
    .unwrap();
    fs::write(
        root.join("hosts/b.kdl"),
        "host \"b\" {\n    system \"x86_64-linux\"\n}\n",
    )
    .unwrap();

    let mut baselines = BTreeMap::new();
    baselines.insert(
        "a".to_string(),
        HostBaseline {
            release: "24.11".into(),
            nixpkgs_rev: "rev-a".into(),
            options_hash: String::new(),
            modules: Vec::new(),
        },
    );
    let lock = Lock {
        version: 1,
        tool: "0.3.1".parse().unwrap(),
        formatter: FormatterPin {
            name: "identity".into(),
            version: "0".into(),
        },
        oracle: OraclePin {
            nixpkgs_rev: "rev-b".into(),
            options_hash: String::new(),
            modules: Vec::new(),
        },
        module_sources: Vec::new(),
        inputs: BTreeMap::new(),
        modules: BTreeMap::new(),
        outputs: Vec::new(),
        pins: BTreeMap::new(),
        baselines,
    };
    fs::write(root.join("knixl.lock.kdl"), lock.render()).unwrap();

    let project = gather(&root, &identity_formatter(), "0.3.1".parse().unwrap()).expect("gather");

    assert!(
        project.oracles.contains_key("a"),
        "host a should have an oracle entry"
    );
    assert!(
        project.oracles.contains_key("b"),
        "host b should have an oracle entry"
    );

    let foo = ident_path(&["services", "foo", "enable"]);
    let bar = ident_path(&["services", "bar", "enable"]);

    // Host a was validated against rev-a's option set: it knows foo, not bar.
    assert!(project.oracles["a"]
        .check(&foo, &NixExpr::Bool(true))
        .is_ok());
    assert!(project.oracles["a"]
        .check(&bar, &NixExpr::Bool(true))
        .is_err());

    // Host b fell back to the lock's default rev-b: it knows bar, not foo.
    assert!(project.oracles["b"]
        .check(&bar, &NixExpr::Bool(true))
        .is_ok());
    assert!(project.oracles["b"]
        .check(&foo, &NixExpr::Bool(true))
        .is_err());

    std::env::remove_var("XDG_CACHE_HOME");
    let _ = fs::remove_dir_all(&tmp);
    let _ = fs::remove_dir_all(&root);
}
