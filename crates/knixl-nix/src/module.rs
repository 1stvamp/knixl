//! Flake-ref resolution for out-of-tree oracle modules (e.g. disko, sops-nix): a declared
//! `github:owner/repo` reference resolved to a pinned `{ url, rev }`, so the module sources
//! feeding the oracle's option set are reproducible alongside nixpkgs itself (#35). Mirrors
//! `baseline.rs`: built-in by default, using `git ls-remote <url> HEAD` for the default
//! branch's head commit, run only when a module needs (re-)resolving.
//! `KNIXL_MODULE_RESOLVER` overrides with an external `<flake-ref>` -> `<commit>` command.
//! A failure to resolve blocks the resolution (never a wrong result).

use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, thiserror::Error)]
pub enum ModuleError {
    #[error("unsupported flake reference: {0}")]
    UnsupportedRef(String),
    #[error("module resolver is not available: {0}")]
    Unavailable(String),
    #[error("module resolver failed: {0}")]
    Failed(String),
}

/// A flake ref resolved to a concrete source: the plain URL a lock entry records, and the
/// commit it currently points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedModule {
    pub url: String,
    pub rev: String,
}

/// The module-source resolver. `KNIXL_MODULE_RESOLVER` selects an external command (the
/// `<flake-ref>` -> `<commit>` protocol); unset uses the built-in `git ls-remote` resolver.
pub enum ModuleResolver {
    External(PathBuf),
    Builtin,
}

impl ModuleResolver {
    pub fn resolve() -> ModuleResolver {
        match std::env::var_os("KNIXL_MODULE_RESOLVER") {
            Some(p) => ModuleResolver::External(PathBuf::from(p)),
            None => ModuleResolver::Builtin,
        }
    }

    /// Resolve a flake ref (e.g. `"github:nix-community/disko"`) to its URL and current
    /// commit. The URL is derived here (not left to either branch below) so an
    /// unsupported/empty ref is refused clearly, before any resolver runs: neither branch
    /// could otherwise notice that `url_from_flake_ref` returned nothing, and would go on to
    /// pin an empty rev under an empty url.
    pub fn lookup(&self, flake_ref: &str) -> Result<ResolvedModule, ModuleError> {
        let url = url_from_flake_ref(flake_ref)
            .ok_or_else(|| ModuleError::UnsupportedRef(flake_ref.to_string()))?;
        let rev = match self {
            ModuleResolver::External(bin) => lookup_external(bin, flake_ref)?,
            ModuleResolver::Builtin => lookup_builtin(&url)?,
        };
        Ok(ResolvedModule { url, rev })
    }
}

/// The external-command protocol: run `<bin> <flake-ref>`, expect `<commit>`.
fn lookup_external(bin: &Path, flake_ref: &str) -> Result<String, ModuleError> {
    let out = crate::output_retrying_etxtbsy(|| {
        let mut c = Command::new(bin);
        c.arg(flake_ref);
        c
    })
    .map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ModuleError::Unavailable(format!("{} not found", bin.display()))
        } else {
            ModuleError::Unavailable(e.to_string())
        }
    })?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let combined = format!("{stderr}{stdout}").trim().to_string();
        return Err(ModuleError::Failed(combined));
    }
    let line = String::from_utf8_lossy(&out.stdout);
    let mut it = line.split_whitespace();
    match (it.next(), it.next()) {
        (Some(rev), None) => Ok(rev.to_string()),
        _ => Err(ModuleError::Failed(format!(
            "resolver output not `<commit>`: {}",
            line.trim()
        ))),
    }
}

/// The built-in resolver: `git ls-remote <url> HEAD` for the default branch's head commit.
fn lookup_builtin(url: &str) -> Result<String, ModuleError> {
    let out = crate::output_retrying_etxtbsy(|| {
        let mut c = Command::new("git");
        c.args(["ls-remote", url, "HEAD"]);
        c
    })
    .map_err(|e| ModuleError::Unavailable(e.to_string()))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(ModuleError::Failed(stderr.trim().to_string()));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    rev_from_ls_remote(&stdout).ok_or_else(|| ModuleError::Failed(format!("no HEAD found: {url}")))
}

/// Maps `github:owner/repo` (optionally followed by `/rev`, `?ref=..`, or similar suffixes)
/// to `https://github.com/owner/repo`. Any other form (a different flake registry prefix, or
/// an owner/repo that fails to parse) returns `None`, so a caller can refuse clearly rather
/// than resolve against an empty or nonsensical URL.
pub fn url_from_flake_ref(flake_ref: &str) -> Option<String> {
    let rest = flake_ref.strip_prefix("github:")?;
    let (owner, repo_and_rest) = rest.split_once('/')?;
    let repo = repo_and_rest.split(['/', '?', '#']).next()?;
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some(format!("https://github.com/{owner}/{repo}"))
}

/// The leading commit from a `git ls-remote` output's first line, before the tab. Mirrors
/// `baseline::rev_from_ls_remote`'s shape, but without the 40-hex-digit check: `ls-remote
/// <url> HEAD` returns exactly one matching line, so there is no sibling-ref ambiguity to
/// guard against the way the baseline's branch-name lookup has.
pub fn rev_from_ls_remote(out: &str) -> Option<String> {
    let line = out.lines().next()?;
    let sha = line.split('\t').next()?;
    if sha.is_empty() {
        None
    } else {
        Some(sha.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    /// A shim mimicking the resolver: prints `stdout_line` and exits `code`.
    fn shim(tag: &str, stdout_line: &str, stderr_line: &str, code: i32) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("knixl-moduleshim-{}-{tag}", std::process::id()));
        let script = format!(
            "#!/bin/sh\n[ -n \"{o}\" ] && echo \"{o}\"\n[ -n \"{e}\" ] && echo \"{e}\" 1>&2\nexit {code}\n",
            o = stdout_line, e = stderr_line,
        );
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(script.as_bytes()).unwrap();
        f.flush().unwrap();
        drop(f);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn github_flake_ref_to_url() {
        assert_eq!(
            url_from_flake_ref("github:nix-community/disko").as_deref(),
            Some("https://github.com/nix-community/disko")
        );
        assert_eq!(
            url_from_flake_ref("github:Mic92/sops-nix").as_deref(),
            Some("https://github.com/Mic92/sops-nix")
        );
    }

    #[test]
    fn flake_ref_with_a_trailing_rev_or_ref_still_maps_to_the_bare_repo_url() {
        assert_eq!(
            url_from_flake_ref("github:owner/repo/some-rev").as_deref(),
            Some("https://github.com/owner/repo")
        );
        assert_eq!(
            url_from_flake_ref("github:owner/repo?ref=main").as_deref(),
            Some("https://github.com/owner/repo")
        );
    }

    #[test]
    fn unsupported_or_empty_flake_refs_are_none() {
        assert_eq!(url_from_flake_ref(""), None);
        assert_eq!(url_from_flake_ref("git+https://example.com/x"), None);
        assert_eq!(url_from_flake_ref("github:"), None);
        assert_eq!(url_from_flake_ref("github:owner"), None);
        assert_eq!(url_from_flake_ref("github:/repo"), None);
    }

    #[test]
    fn ls_remote_head_to_rev() {
        let out = "abc123\tHEAD\ndef\trefs/heads/main\n";
        assert_eq!(rev_from_ls_remote(out).as_deref(), Some("abc123"));
    }

    #[test]
    fn ls_remote_empty_is_none() {
        assert_eq!(rev_from_ls_remote(""), None);
    }

    #[test]
    fn lookup_rejects_an_unsupported_ref_before_running_any_resolver() {
        // A resolver that would fail the test if it were ever invoked (nonexistent binary).
        let r = ModuleResolver::External(PathBuf::from("/nonexistent/knixl-no-such-resolver"));
        assert!(matches!(
            r.lookup("git+https://example.com/x"),
            Err(ModuleError::UnsupportedRef(_))
        ));
    }

    #[test]
    fn lookup_external_ok_parses_the_resolved_module() {
        let r = ModuleResolver::External(shim("ok", "abc123", "", 0));
        let got = r.lookup("github:nix-community/disko").unwrap();
        assert_eq!(got.url, "https://github.com/nix-community/disko");
        assert_eq!(got.rev, "abc123");
    }

    #[test]
    fn lookup_external_failure_is_failed() {
        let r = ModuleResolver::External(shim("fail", "", "boom", 2));
        assert!(matches!(
            r.lookup("github:nix-community/disko"),
            Err(ModuleError::Failed(_))
        ));
    }

    #[test]
    fn lookup_external_missing_binary_is_unavailable() {
        let r = ModuleResolver::External(PathBuf::from("/nonexistent/knixl-no-such-resolver"));
        assert!(matches!(
            r.lookup("github:nix-community/disko"),
            Err(ModuleError::Unavailable(_))
        ));
    }

    #[test]
    fn lookup_external_empty_stdout_is_failed() {
        let r = ModuleResolver::External(shim("bad", "", "", 0));
        assert!(matches!(
            r.lookup("github:nix-community/disko"),
            Err(ModuleError::Failed(_))
        ));
    }

    #[test]
    fn lookup_external_trailing_tokens_is_failed() {
        let r = ModuleResolver::External(shim("trailing", "abc123 extra", "", 0));
        assert!(matches!(
            r.lookup("github:nix-community/disko"),
            Err(ModuleError::Failed(_))
        ));
    }
}
