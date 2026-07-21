//! Validate emitted option paths against the real NixOS option set (nixosOptionsDoc).
//! Best-effort by design: catches unknown paths and gross type mismatches, punts on
//! submodule interiors. The nixpkgs rev is pinned in the lock. SPEC-GRADE SKETCH.

pub mod nixtype;

use knixl_ir::{AttrPath, NixExpr};
use nixtype::NixType;
use std::collections::BTreeMap;
use std::path::Path;

pub struct Oracle {
    options: BTreeMap<String, OptionInfo>,
}

pub struct OptionInfo {
    pub ty: NixType,
    pub has_default: bool,
    pub read_only: bool,
    pub declarations: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum OracleError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[derive(Debug)]
pub enum TypeMismatch {
    UnknownOption {
        key: String,
    },
    ReadOnly {
        key: String,
    },
    WrongType {
        key: String,
        expected: String,
        got: String,
    },
}

mod options_json {
    // serde model of nixosOptionsDoc output. `ty` is a human-readable STRING, which is
    // the whole reason the oracle is best-effort.
    #[derive(serde::Deserialize)]
    pub struct Entry {
        #[serde(rename = "type")]
        pub ty: String,
        pub default: Option<serde_json::Value>,
        #[serde(rename = "readOnly", default)]
        pub read_only: bool,
        #[serde(default)]
        pub declarations: Vec<String>,
    }
    pub type Raw = std::collections::BTreeMap<String, Entry>;
}

impl Oracle {
    /// Build from a cached options.json. The caller pins the rev and caches by it.
    pub fn from_options_json(path: &Path) -> Result<Self, OracleError> {
        let raw: options_json::Raw = serde_json::from_slice(&std::fs::read(path)?)?;
        let options = raw
            .into_iter()
            .map(|(k, v)| {
                (
                    k,
                    OptionInfo {
                        ty: NixType::parse_description(&v.ty),
                        has_default: v.default.is_some(),
                        read_only: v.read_only,
                        declarations: v.declarations,
                    },
                )
            })
            .collect();
        Ok(Self { options })
    }

    /// Load the options set cached for a pinned nixpkgs rev, if it has been fetched. Returns
    /// `Ok(None)` when the rev is empty or nothing is cached for it, so generation proceeds
    /// without option checks (best-effort, same as an absent options file).
    pub fn from_rev_cache(rev: &str) -> Result<Option<Self>, OracleError> {
        match cache_path(rev) {
            Some(p) if p.is_file() => Self::from_options_json(&p).map(Some),
            _ => Ok(None),
        }
    }

    /// Check one emitted assignment. Submodule interiors are left unchecked (Ok).
    pub fn check(&self, path: &AttrPath, value: &NixExpr) -> Result<(), TypeMismatch> {
        let key = path.to_option_key(); // dynamic keys collapsed to <name>
        match self.options.get(&key) {
            // Not a leaf option: accept if it is the root of a submodule (an attrset whose
            // children are known options, e.g. services.restic.backups.<name>); the interior
            // is left unchecked. A genuine typo has no known children and is still rejected.
            None if self.is_option_prefix(&key) => Ok(()),
            None => Err(TypeMismatch::UnknownOption { key }),
            Some(info) if info.read_only => Err(TypeMismatch::ReadOnly { key }),
            Some(info) => info
                .ty
                .accepts(value)
                .map_err(|expected| TypeMismatch::WrongType {
                    key,
                    expected,
                    got: value_kind(value),
                }),
        }
    }

    /// True if `key` is a strict prefix of some known option path (so `key` names an
    /// intermediate attribute set that contains real options).
    fn is_option_prefix(&self, key: &str) -> bool {
        let prefix = format!("{key}.");
        self.options.keys().any(|k| k.starts_with(&prefix))
    }
}

/// `$XDG_CACHE_HOME/knixl` (falling back to `$HOME/.cache/knixl`). Shared by `cache_path`
/// and `cache_path_for` so both key schemes land in the same directory.
fn cache_dir() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cache")))?;
    Some(base.join("knixl"))
}

/// Where a fetched options.json lives for a given nixpkgs rev:
/// `$XDG_CACHE_HOME/knixl/options-<rev>.json` (falling back to `$HOME/.cache`). Returns
/// None for an empty rev or when no cache/home directory can be determined.
pub fn cache_path(rev: &str) -> Option<std::path::PathBuf> {
    if rev.is_empty() {
        return None;
    }
    Some(cache_dir()?.join(format!("options-{rev}.json")))
}

/// The canonical, order-sensitive string for an effective set (a nixpkgs rev plus module
/// pins): `rev`, then each module as `url@rev#attr`, one per line, in slice order.
fn effective_set_key(rev: &str, modules: &[(String, String, String)]) -> String {
    let mut key = rev.to_string();
    for (url, module_rev, attr) in modules {
        key.push('\n');
        key.push_str(url);
        key.push('@');
        key.push_str(module_rev);
        key.push('#');
        key.push_str(attr);
    }
    key
}

/// The blake3 hash of an effective set (nixpkgs rev plus module pins). Order-sensitive: two
/// module lists with the same entries in a different order hash differently, since a
/// different declaration order can change how later modules override earlier ones.
pub fn effective_hash(rev: &str, modules: &[(String, String, String)]) -> String {
    // blake3 directly, so the oracle stays a leaf library (no dep on knixl-nix, per the crate
    // graph in CLAUDE.md). Same "blake3:<hex>" form knixl-nix produces.
    let h = blake3::hash(effective_set_key(rev, modules).as_bytes());
    format!("blake3:{}", h.to_hex())
}

/// Cache path for an effective set: the rev-only path (base compat, matches `cache_path`)
/// when `modules` is empty, else `options-<effective-hash>.json` under the same cache
/// directory. The hash's `blake3:` prefix is stripped before use as a filename, since blake3
/// hex digits alone are already filesystem-safe.
pub fn cache_path_for(
    rev: &str,
    modules: &[(String, String, String)],
) -> Option<std::path::PathBuf> {
    if modules.is_empty() {
        return cache_path(rev);
    }
    let hash = effective_hash(rev, modules);
    let hex = hash.strip_prefix("blake3:").unwrap_or(&hash);
    Some(cache_dir()?.join(format!("options-{hex}.json")))
}

fn value_kind(v: &NixExpr) -> String {
    use NixExpr::*;
    match v {
        Bool(_) => "boolean",
        Int(_) => "integer",
        Float(_) => "floating point number",
        Str(_) | IndentStr(_) => "string",
        Path(_) => "path",
        Null => "null",
        List(_) => "list",
        AttrSet(_) => "attribute set",
        _ => "expression",
    }
    .to_string()
}
