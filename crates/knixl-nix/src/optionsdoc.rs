//! `nixosOptionsDoc` build expression: fetches nixpkgs at a pinned rev, evaluates the NixOS
//! module system over a minimal base module plus any declared out-of-tree module pins, and
//! returns the resulting `options.json` text. Empty module pins reproduce nixpkgs' base
//! option set, which is also how the base cache entry (`knixl_oracle::cache_path`) gets
//! (re-)built. Nix-version sensitive: this is the expression that needs iterating against a
//! real nix, the base (no-modules) path first.

use crate::nixeval::{NixError, NixEval};
use std::path::Path;

/// Escape a string for embedding in a double-quoted Nix string literal: backslashes, quotes,
/// and `${` (which would otherwise start an interpolation).
fn nix_string_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace("${", "\\${")
}

/// One declared module pin, rendered as `(builtins.getFlake "git+<url>?rev=<rev>").nixosModules."<attr>"`.
fn module_import_expr(url: &str, rev: &str, attr: &str) -> String {
    let url = nix_string_escape(url);
    let rev = nix_string_escape(rev);
    let attr = nix_string_escape(attr);
    format!("(builtins.getFlake \"git+{url}?rev={rev}\").nixosModules.\"{attr}\"")
}

/// The build expression: fetch nixpkgs at `nixpkgs_rev`, evaluate `nixos/lib/eval-config.nix`
/// with a minimal `hostPlatform` plus each declared module, and select `nixosOptionsDoc`'s
/// `optionsJSON` output derivation (a directory containing `share/doc/nixos/options.json`).
fn options_doc_expr(nixpkgs_rev: &str, modules: &[(String, String, String)]) -> String {
    let rev = nix_string_escape(nixpkgs_rev);
    let module_imports = modules
        .iter()
        .map(|(url, mrev, attr)| module_import_expr(url, mrev, attr))
        .collect::<Vec<_>>()
        .join("\n      ");

    format!(
        r#"let
  nixpkgsSrc = builtins.fetchGit {{
    url = "https://github.com/NixOS/nixpkgs";
    rev = "{rev}";
    shallow = true;
  }};
  eval = import (nixpkgsSrc + "/nixos/lib/eval-config.nix") {{
    system = null;
    modules = [
      {{ nixpkgs.hostPlatform = "x86_64-linux"; }}
      {module_imports}
    ];
  }};
in
(eval.pkgs.nixosOptionsDoc {{ inherit (eval) options; }}).optionsJSON
"#
    )
}

/// Build `options.json` for a nixpkgs rev plus module pins. Empty pins reproduce nixpkgs'
/// base option set (also the base-build automation). Builds the `nixosOptionsDoc` expression
/// via `eval`'s build binary (mirroring `NixEval::builds_expr`'s command shape), then reads
/// the produced `share/doc/nixos/options.json` out of the built store path.
pub fn build_options_json(
    eval: &NixEval,
    nixpkgs_rev: &str,
    modules: &[(String, String, String)],
) -> Result<String, NixError> {
    let expr = options_doc_expr(nixpkgs_rev, modules);
    let out = crate::output_retrying_etxtbsy(|| {
        let mut c = std::process::Command::new(&eval.build_bin);
        c.args([
            "--no-out-link",
            "--extra-experimental-features",
            "nix-command flakes",
            "-E",
            &expr,
        ]);
        c
    })
    .map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            NixError::Unavailable(format!("{} not found", eval.build_bin.display()))
        } else {
            NixError::Unavailable(e.to_string())
        }
    })?;
    if !out.status.success() {
        return Err(NixError::Failed(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ));
    }
    let store_path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let json_path = Path::new(&store_path).join("share/doc/nixos/options.json");
    std::fs::read_to_string(&json_path)
        .map_err(|e| NixError::Failed(format!("reading {}: {e}", json_path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expr_embeds_the_pinned_rev_and_shallow_fetch() {
        let expr = options_doc_expr("abc123", &[]);
        assert!(expr.contains("rev = \"abc123\";"));
        assert!(expr.contains("shallow = true;"));
        assert!(expr.contains("nixosOptionsDoc"));
    }

    #[test]
    fn expr_with_no_modules_has_only_the_base_hostplatform_module() {
        let expr = options_doc_expr("abc123", &[]);
        assert!(!expr.contains("getFlake"));
    }

    #[test]
    fn expr_embeds_each_module_pin_as_a_flake_nixos_module() {
        let expr = options_doc_expr(
            "abc123",
            &[
                (
                    "https://github.com/o/r1".into(),
                    "rev1".into(),
                    "default".into(),
                ),
                (
                    "https://github.com/o/r2".into(),
                    "rev2".into(),
                    "custom".into(),
                ),
            ],
        );
        assert!(expr.contains(
            "(builtins.getFlake \"git+https://github.com/o/r1?rev=rev1\").nixosModules.\"default\""
        ));
        assert!(expr.contains(
            "(builtins.getFlake \"git+https://github.com/o/r2?rev=rev2\").nixosModules.\"custom\""
        ));
    }

    #[test]
    fn nix_string_escape_handles_quotes_backslashes_and_interpolation() {
        assert_eq!(nix_string_escape(r#"a"b"#), r#"a\"b"#);
        assert_eq!(nix_string_escape(r"a\b"), r"a\\b");
        assert_eq!(nix_string_escape("a${b}"), r"a\${b}");
    }

    /// True if a nix build binary is actually usable in this environment, so the
    /// integration test below can skip cleanly (rather than fail CI) when nix is absent.
    fn nix_build_available(eval: &NixEval) -> bool {
        std::process::Command::new(&eval.build_bin)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// nix-gated: builds the real base (no-modules) options.json against a small pinned
    /// nixpkgs rev and checks the result is valid, non-empty options.json (parseable the
    /// same way `knixl_oracle::Oracle::from_options_json` parses it: a JSON object keyed by
    /// option path, each entry carrying at least a `type` string). Skips cleanly, rather
    /// than failing, when nix is unavailable (mirrors the golden/formatter tests' gating).
    #[test]
    fn builds_real_base_options_json_against_a_pinned_nixpkgs_rev() {
        let eval = NixEval::resolve();
        if !nix_build_available(&eval) {
            eprintln!("skipping: nix build binary not available");
            return;
        }

        // A real nixos-25.05 branch head, pinned here so the test is reproducible.
        let rev = "ac62194c3917d5f474c1a844b6fd6da2db95077d";
        let json = match build_options_json(&eval, rev, &[]) {
            Ok(json) => json,
            Err(e) => panic!("build_options_json failed against real nix: {e}"),
        };

        let parsed: serde_json::Value =
            serde_json::from_str(&json).expect("options.json is valid JSON");
        let obj = parsed.as_object().expect("options.json is a JSON object");
        assert!(!obj.is_empty(), "options.json has entries");

        // A well-known, stable option that should exist under any base NixOS eval.
        let known = obj
            .get("system.stateVersion")
            .expect("system.stateVersion is a known base option");
        assert!(
            known.get("type").and_then(|t| t.as_str()).is_some(),
            "entry carries a `type` string, matching Oracle::from_options_json's contract"
        );
    }

    /// nix-gated: as above, but with one real out-of-tree module pin (disko, a small,
    /// widely-used flake module), proving `builtins.getFlake ... .nixosModules.<attr>`
    /// actually resolves and its options land in the produced `options.json` alongside
    /// nixpkgs' own. Skips cleanly when nix is unavailable.
    #[test]
    fn builds_options_json_with_one_real_module_pin() {
        let eval = NixEval::resolve();
        if !nix_build_available(&eval) {
            eprintln!("skipping: nix build binary not available");
            return;
        }

        let nixpkgs_rev = "ac62194c3917d5f474c1a844b6fd6da2db95077d"; // nixos-25.05 branch head
        let modules = [(
            "https://github.com/nix-community/disko".to_string(),
            "ff8702b4de27f72b4c78573dfb89ec74e36abdf1".to_string(),
            "disko".to_string(),
        )];
        let json = match build_options_json(&eval, nixpkgs_rev, &modules) {
            Ok(json) => json,
            Err(e) => panic!("build_options_json with a module pin failed against real nix: {e}"),
        };

        let parsed: serde_json::Value =
            serde_json::from_str(&json).expect("options.json is valid JSON");
        let obj = parsed.as_object().expect("options.json is a JSON object");
        assert!(
            obj.keys().any(|k| k.starts_with("disko.")),
            "expected a disko.* option from the pinned module, found none among {} keys",
            obj.len()
        );
    }
}
