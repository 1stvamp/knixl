//! Release-to-commit resolution for a host's baseline nixpkgs rev (e.g. `"25.05"` ->
//! a nixpkgs commit). Mirrors `pin.rs`: built-in by default, using `git ls-remote` against
//! the `nixos-<release>` branch (falling back to the GitHub commits API if `git` is
//! unavailable or fails), run only when a baseline needs (re-)resolving. `KNIXL_BASELINE_RESOLVER`
//! overrides with an external `<name> <release>` -> `<commit>` command. A failure to resolve
//! blocks the resolution (never a wrong result).

use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, thiserror::Error)]
pub enum BaselineError {
    #[error("baseline resolver is not available: {0}")]
    Unavailable(String),
    #[error("no nixpkgs commit found: {0}")]
    NotFound(String),
    #[error("baseline resolver failed: {0}")]
    Failed(String),
}

/// The baseline resolver. `KNIXL_BASELINE_RESOLVER` selects an external command (the
/// `<bin> <release>` protocol); unset uses the built-in `git ls-remote`/GitHub API resolver.
pub enum BaselineResolver {
    External(PathBuf),
    Builtin,
}

impl BaselineResolver {
    pub fn resolve() -> BaselineResolver {
        match std::env::var_os("KNIXL_BASELINE_RESOLVER") {
            Some(p) => BaselineResolver::External(PathBuf::from(p)),
            None => BaselineResolver::Builtin,
        }
    }

    /// Resolve a NixOS release channel (e.g. `"25.05"`) to a nixpkgs commit.
    pub fn lookup(&self, release: &str) -> Result<String, BaselineError> {
        match self {
            BaselineResolver::External(bin) => lookup_external(bin, release),
            BaselineResolver::Builtin => lookup_builtin(release),
        }
    }
}

/// The external-command protocol: run `<bin> <release>`, expect `<commit>`.
fn lookup_external(bin: &Path, release: &str) -> Result<String, BaselineError> {
    let out = crate::output_retrying_etxtbsy(|| {
        let mut c = Command::new(bin);
        c.arg(release);
        c
    })
    .map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            BaselineError::Unavailable(format!("{} not found", bin.display()))
        } else {
            BaselineError::Unavailable(e.to_string())
        }
    })?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let combined = format!("{}{}", stderr, stdout).trim().to_string();
        if combined.to_lowercase().contains("not found") {
            return Err(BaselineError::NotFound(format!("{release}: {combined}")));
        }
        let err_msg = if !stderr.is_empty() {
            stderr.trim().to_string()
        } else {
            stdout.trim().to_string()
        };
        return Err(BaselineError::Failed(err_msg));
    }
    let line = String::from_utf8_lossy(&out.stdout);
    let mut it = line.split_whitespace();
    match (it.next(), it.next()) {
        (Some(rev), None) => Ok(rev.to_string()),
        _ => Err(BaselineError::Failed(format!(
            "resolver output not `<commit>`: {}",
            line.trim()
        ))),
    }
}

/// The built-in resolver: `git ls-remote` for the `nixos-<release>` branch head, falling back
/// to the GitHub commits API when git is unavailable or fails.
fn lookup_builtin(release: &str) -> Result<String, BaselineError> {
    let branch_ref = format!("refs/heads/nixos-{release}");
    let git_result = crate::output_retrying_etxtbsy(|| {
        let mut c = Command::new("git");
        c.args(["ls-remote", "https://github.com/NixOS/nixpkgs", &branch_ref]);
        c
    });

    match git_result {
        Ok(out) if out.status.success() => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            match rev_from_ls_remote(&stdout) {
                Some(rev) => Ok(rev),
                None => lookup_github(release),
            }
        }
        _ => lookup_github(release),
    }
}

/// The GitHub commits-API fallback for a `nixos-<release>` branch head.
fn lookup_github(release: &str) -> Result<String, BaselineError> {
    let url = format!("https://api.github.com/repos/NixOS/nixpkgs/commits/nixos-{release}");
    let body = match ureq::get(&url).header("User-Agent", "knixl").call() {
        Ok(mut resp) => resp
            .body_mut()
            .read_to_string()
            .map_err(|e| BaselineError::Failed(format!("reading GitHub API response: {e}")))?,
        Err(ureq::Error::StatusCode(404)) => {
            return Err(BaselineError::NotFound(format!(
                "{release}: no nixos-{release} branch found"
            )))
        }
        Err(ureq::Error::StatusCode(code)) => {
            return Err(BaselineError::Unavailable(format!(
                "GitHub API returned HTTP {code}"
            )))
        }
        Err(e) => {
            return Err(BaselineError::Unavailable(format!(
                "GitHub API unreachable: {e}"
            )))
        }
    };
    rev_from_github_json(&body).ok_or_else(|| {
        BaselineError::Failed(format!(
            "GitHub API response has no sha: {}",
            body.chars().take(200).collect::<String>()
        ))
    })
}

/// The first line's leading 40-hex SHA from `git ls-remote` output, before the tab.
pub fn rev_from_ls_remote(out: &str) -> Option<String> {
    let line = out.lines().next()?;
    let sha = line.split('\t').next()?;
    if sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        Some(sha.to_string())
    } else {
        None
    }
}

/// The top-level `sha` string from a GitHub commits-API response body.
pub fn rev_from_github_json(json: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()?
        .get("sha")?
        .as_str()
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    /// A shim mimicking the resolver: prints `stdout_line` and exits `code`.
    fn shim(tag: &str, stdout_line: &str, stderr_line: &str, code: i32) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("knixl-baselineshim-{}-{tag}", std::process::id()));
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
    fn lookup_ok_parses_commit() {
        let r = BaselineResolver::External(shim("ok", "abc123", "", 0));
        let got = r.lookup("25.05").unwrap();
        assert_eq!(got, "abc123");
    }

    #[test]
    fn lookup_not_found_maps_to_notfound() {
        let r = BaselineResolver::External(shim("nf", "", "release not found", 1));
        assert!(matches!(r.lookup("99.99"), Err(BaselineError::NotFound(_))));
    }

    #[test]
    fn lookup_other_failure_maps_to_failed() {
        let r = BaselineResolver::External(shim("fail", "", "boom", 2));
        assert!(matches!(r.lookup("25.05"), Err(BaselineError::Failed(_))));
    }

    #[test]
    fn lookup_missing_binary_is_unavailable() {
        let r = BaselineResolver::External(PathBuf::from("/nonexistent/knixl-no-such-resolver"));
        assert!(matches!(
            r.lookup("25.05"),
            Err(BaselineError::Unavailable(_))
        ));
    }

    #[test]
    fn lookup_empty_stdout_is_failed() {
        let r = BaselineResolver::External(shim("bad", "", "", 0));
        assert!(matches!(r.lookup("25.05"), Err(BaselineError::Failed(_))));
    }

    #[test]
    fn lookup_trailing_tokens_is_failed() {
        let r = BaselineResolver::External(shim("trailing", "abc123 extra", "", 0));
        assert!(matches!(r.lookup("25.05"), Err(BaselineError::Failed(_))));
    }

    const SAMPLE_LS_REMOTE: &str =
        "5629520edecb69630a3f4d17d3d33fc96c13f6fe\trefs/heads/nixos-25.05\n";

    #[test]
    fn rev_from_ls_remote_reads_the_leading_sha() {
        assert_eq!(
            rev_from_ls_remote(SAMPLE_LS_REMOTE).as_deref(),
            Some("5629520edecb69630a3f4d17d3d33fc96c13f6fe")
        );
    }

    #[test]
    fn rev_from_ls_remote_empty_is_none() {
        assert_eq!(rev_from_ls_remote(""), None);
    }

    #[test]
    fn rev_from_ls_remote_garbage_is_none() {
        assert_eq!(rev_from_ls_remote("not a valid line\n"), None);
        assert_eq!(
            rev_from_ls_remote("tooshortsha\trefs/heads/nixos-25.05\n"),
            None
        );
    }

    const SAMPLE_GITHUB_COMMIT: &str = r#"{
        "sha": "5629520edecb69630a3f4d17d3d33fc96c13f6fe",
        "commit": {
            "message": "Merge branch 'staging-25.05'",
            "author": { "name": "someone", "date": "2026-01-01T00:00:00Z" }
        },
        "html_url": "https://github.com/NixOS/nixpkgs/commit/5629520edecb69630a3f4d17d3d33fc96c13f6fe"
    }"#;

    #[test]
    fn rev_from_github_json_reads_the_top_level_sha() {
        assert_eq!(
            rev_from_github_json(SAMPLE_GITHUB_COMMIT).as_deref(),
            Some("5629520edecb69630a3f4d17d3d33fc96c13f6fe")
        );
    }

    #[test]
    fn rev_from_github_json_missing_sha_is_none() {
        assert_eq!(
            rev_from_github_json(r#"{"commit": {"message": "hi"}}"#),
            None
        );
    }

    #[test]
    fn rev_from_github_json_malformed_is_none() {
        assert_eq!(rev_from_github_json("not json"), None);
        assert_eq!(rev_from_github_json(""), None);
    }
}
