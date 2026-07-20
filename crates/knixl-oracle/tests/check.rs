//! Oracle checks against option data: a small committed fixture (always run) and, when
//! `KNIXL_OPTIONS_JSON` points at a real nixosOptionsDoc file, that too.

use std::path::{Path, PathBuf};

use knixl_ir::{AttrKey, AttrPath, NixExpr};
use knixl_oracle::{Oracle, TypeMismatch};

fn ident_path(segs: &[&str]) -> AttrPath {
    AttrPath(
        segs.iter()
            .map(|s| AttrKey::Ident((*s).to_string()))
            .collect(),
    )
}

fn fixture() -> Oracle {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/options.json");
    Oracle::from_options_json(&path).expect("load fixture")
}

#[test]
fn accepts_a_known_option_of_the_right_type() {
    let oracle = fixture();
    assert!(oracle
        .check(
            &ident_path(&["services", "nginx", "enable"]),
            &NixExpr::Bool(true)
        )
        .is_ok());
}

#[test]
fn rejects_unknown_option_paths() {
    let oracle = fixture();
    let err = oracle
        .check(
            &ident_path(&["services", "nginx", "bogusOption"]),
            &NixExpr::Bool(true),
        )
        .unwrap_err();
    assert!(matches!(err, TypeMismatch::UnknownOption { .. }));
}

#[test]
fn rejects_a_gross_type_mismatch() {
    let oracle = fixture();
    let err = oracle
        .check(
            &ident_path(&["services", "nginx", "enable"]),
            &NixExpr::Str("yes".into()),
        )
        .unwrap_err();
    assert!(matches!(err, TypeMismatch::WrongType { .. }));
}

#[test]
fn rejects_writes_to_read_only_options() {
    let oracle = fixture();
    let err = oracle
        .check(
            &ident_path(&["system", "stateVersion"]),
            &NixExpr::Str("25.11".into()),
        )
        .unwrap_err();
    assert!(matches!(err, TypeMismatch::ReadOnly { .. }));
}

#[test]
fn collapses_dynamic_keys_to_name_for_lookup() {
    let oracle = fixture();
    // services.nginx.virtualHosts."example.com".forceSSL -> ...virtualHosts.<name>.forceSSL
    let path = AttrPath(vec![
        AttrKey::Ident("services".into()),
        AttrKey::Ident("nginx".into()),
        AttrKey::Ident("virtualHosts".into()),
        AttrKey::Quoted("example.com".into()),
        AttrKey::Ident("forceSSL".into()),
    ]);
    assert!(oracle.check(&path, &NixExpr::Bool(true)).is_ok());
}

#[test]
fn accepts_a_submodule_root_that_has_known_children() {
    let oracle = fixture();
    // services.nginx.virtualHosts.<name> is not a leaf, but it is the root of a submodule
    // whose children (forceSSL, serverAliases) are known options.
    let path = AttrPath(vec![
        AttrKey::Ident("services".into()),
        AttrKey::Ident("nginx".into()),
        AttrKey::Ident("virtualHosts".into()),
        AttrKey::Quoted("example.com".into()),
    ]);
    assert!(oracle
        .check(&path, &NixExpr::AttrSet(Default::default()))
        .is_ok());
}

#[test]
fn from_rev_cache_loads_options_keyed_by_rev() {
    let tmp = std::env::temp_dir().join(format!("knixl-oracle-cache-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::env::set_var("XDG_CACHE_HOME", &tmp);

    // Nothing cached yet, and an empty rev, both resolve to None (best-effort skip).
    assert!(knixl_oracle::Oracle::from_rev_cache("")
        .expect("empty rev")
        .is_none());
    let rev = "deadbeefcafef00d";
    assert!(knixl_oracle::Oracle::from_rev_cache(rev)
        .expect("miss")
        .is_none());

    // Populate the cache at the rev's path, then it loads and checks.
    let dest = knixl_oracle::cache_path(rev).expect("cache path under XDG_CACHE_HOME");
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/options.json"),
        &dest,
    )
    .unwrap();

    let oracle = knixl_oracle::Oracle::from_rev_cache(rev)
        .expect("load")
        .expect("cached present");
    assert!(oracle
        .check(
            &ident_path(&["services", "nginx", "enable"]),
            &NixExpr::Bool(true)
        )
        .is_ok());

    std::env::remove_var("XDG_CACHE_HOME");
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn real_options_json_loads_when_provided() {
    let Ok(path) = std::env::var("KNIXL_OPTIONS_JSON") else {
        eprintln!("skipping: set KNIXL_OPTIONS_JSON to a real nixosOptionsDoc file");
        return;
    };
    let oracle = Oracle::from_options_json(&PathBuf::from(path)).expect("load real options.json");

    // A real, well-known boolean option is accepted.
    assert!(oracle
        .check(
            &ident_path(&["services", "nginx", "enable"]),
            &NixExpr::Bool(true)
        )
        .is_ok());
    // A path that certainly does not exist is rejected.
    let err = oracle
        .check(
            &ident_path(&["services", "nginx", "totallyBogusXyz"]),
            &NixExpr::Bool(true),
        )
        .unwrap_err();
    assert!(matches!(err, TypeMismatch::UnknownOption { .. }));
}
