//! Best-effort nix checks for `knixl install`: does a package attribute exist, and does
//! the generated file parse as Nix. Both shell out to `nix-instantiate`, injectable via
//! `KNIXL_NIX` so tests run against a shim. Full semantic evaluation of a partial NixOS
//! module is deliberately not attempted: a host with a `lib.mkIf config.*` block forces
//! `config`, which a standalone stub cannot satisfy, so it would report false failures.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Which nixpkgs the check resolves against.
#[derive(Debug, Clone)]
pub enum Nixpkgs {
    /// A pinned commit, fetched reproducibly (matches the lock's oracle rev).
    PinnedRev(String),
    /// The caller's ambient `<nixpkgs>` (channel or flake registry).
    Ambient,
}

impl Nixpkgs {
    /// A Nix expression that evaluates to the package set.
    fn expr(&self) -> String {
        match self {
            Nixpkgs::PinnedRev(rev) => format!(
                "import (builtins.fetchTarball \"https://github.com/NixOS/nixpkgs/archive/{rev}.tar.gz\") {{}}"
            ),
            Nixpkgs::Ambient => "import <nixpkgs> {}".to_string(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum NixError {
    /// `nix-instantiate` is not available (not on PATH / cannot spawn).
    #[error("nix is not available: {0}")]
    Unavailable(String),
    /// The tool ran but reported a failure (bad expression, parse error, etc.).
    #[error("nix check failed: {0}")]
    Failed(String),
}

/// A handle to `nix-instantiate`. `KNIXL_NIX` overrides the binary (a shim in tests).
#[derive(Debug, Clone)]
pub struct NixEval {
    pub bin: PathBuf,
}

impl NixEval {
    /// Resolve the checker: `KNIXL_NIX` if set, else `nix-instantiate` on PATH.
    pub fn resolve() -> NixEval {
        let bin = std::env::var_os("KNIXL_NIX")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("nix-instantiate"));
        NixEval { bin }
    }

    fn run(&self, args: &[&str]) -> Result<std::process::Output, NixError> {
        Command::new(&self.bin).args(args).output().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                NixError::Unavailable(format!("{} not found", self.bin.display()))
            } else {
                NixError::Unavailable(e.to_string())
            }
        })
    }

    /// True if `pkgs.<name>` exists in the given nixpkgs.
    pub fn package_exists(&self, src: &Nixpkgs, name: &str) -> Result<bool, NixError> {
        let expr = format!("builtins.hasAttr \"{name}\" ({})", src.expr());
        let out = self.run(&["--eval", "-E", &expr])?;
        if !out.status.success() {
            return Err(NixError::Failed(String::from_utf8_lossy(&out.stderr).trim().to_string()));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim() == "true")
    }

    /// Confirm a generated file parses as a Nix expression.
    pub fn parses(&self, file: &Path) -> Result<(), NixError> {
        let out = self.run(&["--parse", &file.display().to_string()])?;
        if out.status.success() {
            Ok(())
        } else {
            Err(NixError::Failed(String::from_utf8_lossy(&out.stderr).trim().to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    /// A shim mimicking `nix-instantiate`: `--eval -E <expr>` echoes `verdict`; `--parse`
    /// exits with `parse_ok`.
    fn shim(tag: &str, verdict: &str, parse_ok: bool) -> PathBuf {
        let path = std::env::temp_dir().join(format!("knixl-nixshim-{}-{tag}", std::process::id()));
        let parse_exit = if parse_ok { 0 } else { 1 };
        let script = format!(
            "#!/bin/sh\ncase \"$1\" in\n  --eval) echo \"{verdict}\" ;;\n  --parse) exit {parse_exit} ;;\nesac\n"
        );
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(script.as_bytes()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn package_exists_true_when_shim_says_true() {
        let e = NixEval { bin: shim("exists", "true", true) };
        assert!(e.package_exists(&Nixpkgs::Ambient, "ripgrep").unwrap());
    }

    #[test]
    fn package_exists_false_when_shim_says_false() {
        let e = NixEval { bin: shim("missing", "false", true) };
        assert!(!e.package_exists(&Nixpkgs::Ambient, "nope").unwrap());
    }

    #[test]
    fn parses_ok_and_error() {
        let ok = NixEval { bin: shim("parseok", "true", true) };
        assert!(ok.parses(Path::new("/tmp/whatever.nix")).is_ok());
        let bad = NixEval { bin: shim("parsebad", "true", false) };
        assert!(matches!(bad.parses(Path::new("/tmp/whatever.nix")), Err(NixError::Failed(_))));
    }

    #[test]
    fn missing_binary_is_unavailable_not_failure() {
        let e = NixEval { bin: PathBuf::from("/nonexistent/knixl-no-such-nix") };
        assert!(matches!(
            e.package_exists(&Nixpkgs::Ambient, "x"),
            Err(NixError::Unavailable(_))
        ));
    }

    #[test]
    fn pinned_rev_expr_fetches_that_rev() {
        let src = Nixpkgs::PinnedRev("abc123".into());
        assert!(src.expr().contains("archive/abc123.tar.gz"));
    }
}
