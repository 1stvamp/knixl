//! Version-to-commit resolution for `knixl install pkg@version`. Built-in by default: queries
//! the nixhub/devbox version index over HTTPS via ureq (honouring `HTTP_PROXY`/`HTTPS_PROXY`/`NO_PROXY`)
//! and prefetches the sha via nix-prefetch-url (nix must be present); run only at pin time.
//! `KNIXL_PIN_RESOLVER` overrides with an external `<name> <version>` -> `<commit> <sha256>` command.
//! A failure to resolve blocks the pin (never a wrong result).

use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub nixpkgs_rev: String,
    pub sha256: String,
}

#[derive(Debug, thiserror::Error)]
pub enum PinError {
    #[error("version resolver is not available: {0}")]
    Unavailable(String),
    #[error("no nixpkgs commit found: {0}")]
    NotFound(String),
    #[error("version resolver failed: {0}")]
    Failed(String),
}

/// The version resolver. `KNIXL_PIN_RESOLVER` selects an external command (the
/// `<bin> <name> <version>` protocol); unset uses the built-in nixhub/devbox resolver.
pub enum PinResolver {
    External(PathBuf),
    Builtin,
}

impl PinResolver {
    pub fn resolve() -> PinResolver {
        match std::env::var_os("KNIXL_PIN_RESOLVER") {
            Some(p) => PinResolver::External(PathBuf::from(p)),
            None => PinResolver::Builtin,
        }
    }

    /// Resolve `pkgs.<name>` at `version` to a nixpkgs commit and its sha256.
    pub fn lookup(&self, name: &str, version: &str) -> Result<Resolved, PinError> {
        match self {
            PinResolver::External(bin) => lookup_external(bin, name, version),
            PinResolver::Builtin => lookup_builtin(name, version),
        }
    }
}

/// The external-command protocol: run `<bin> <name> <version>`, expect `<commit> <sha256>`.
fn lookup_external(bin: &Path, name: &str, version: &str) -> Result<Resolved, PinError> {
    let out = crate::output_retrying_etxtbsy(|| {
        let mut c = Command::new(bin);
        c.args([name, version]);
        c
    })
    .map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            PinError::Unavailable(format!("{} not found", bin.display()))
        } else {
            PinError::Unavailable(e.to_string())
        }
    })?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let combined = format!("{}{}", stderr, stdout).trim().to_string();
        if combined.to_lowercase().contains("not found") {
            return Err(PinError::NotFound(format!("{name} {version}: {combined}")));
        }
        let err_msg =
            if !stderr.is_empty() { stderr.trim().to_string() } else { stdout.trim().to_string() };
        return Err(PinError::Failed(err_msg));
    }
    let line = String::from_utf8_lossy(&out.stdout);
    let mut it = line.split_whitespace();
    match (it.next(), it.next(), it.next()) {
        (Some(rev), Some(sha), None) => {
            Ok(Resolved { nixpkgs_rev: rev.to_string(), sha256: sha.to_string() })
        }
        _ => Err(PinError::Failed(format!("resolver output not `<commit> <sha256>`: {}", line.trim()))),
    }
}

/// The built-in resolver: nixhub/devbox resolve API for the commit, `nix-prefetch-url` for
/// the sha256. ureq's default agent honours the *_PROXY environment variables.
fn lookup_builtin(name: &str, version: &str) -> Result<Resolved, PinError> {
    let url = format!(
        "https://search.devbox.sh/v1/resolve?name={}&version={}",
        urlencode(name),
        urlencode(version),
    );
    let body = match ureq::get(&url).call() {
        Ok(mut resp) => resp
            .body_mut()
            .read_to_string()
            .map_err(|e| PinError::Failed(format!("reading version index response: {e}")))?,
        Err(ureq::Error::StatusCode(404)) => {
            return Err(PinError::NotFound(format!(
                "{name} {version}: no nixpkgs commit ships that version"
            )))
        }
        Err(ureq::Error::StatusCode(code)) => {
            return Err(PinError::Unavailable(format!("version index returned HTTP {code}")))
        }
        Err(e) => return Err(PinError::Unavailable(format!("version index unreachable: {e}"))),
    };
    let commit = commit_hash(&body).ok_or_else(|| {
        PinError::Failed(format!(
            "version index response has no commit_hash: {}",
            body.chars().take(200).collect::<String>()
        ))
    })?;
    let sha = prefetch_sha(&commit)?;
    Ok(Resolved { nixpkgs_rev: commit, sha256: sha })
}

/// The top-level `commit_hash` string from a resolve-API response body.
fn commit_hash(json: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()?
        .get("commit_hash")?
        .as_str()
        .map(str::to_string)
}

/// Prefetch the sha256 of a nixpkgs tarball at `commit` via `nix-prefetch-url --unpack`.
/// Its base32 output is used verbatim as the `fetchTarball` sha256.
fn prefetch_sha(commit: &str) -> Result<String, PinError> {
    let url = format!("https://github.com/NixOS/nixpkgs/archive/{commit}.tar.gz");
    let out = crate::output_retrying_etxtbsy(|| {
        let mut c = Command::new("nix-prefetch-url");
        c.args(["--unpack", &url]);
        c
    })
    .map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            PinError::Unavailable("nix-prefetch-url not found".into())
        } else {
            PinError::Unavailable(e.to_string())
        }
    })?;
    if !out.status.success() {
        return Err(PinError::Failed(String::from_utf8_lossy(&out.stderr).trim().to_string()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Minimal percent-encoding for query values (name/version are simple, but encode to be safe).
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    /// A shim mimicking the resolver: prints `stdout_line` and exits `code`.
    fn shim(tag: &str, stdout_line: &str, stderr_line: &str, code: i32) -> PathBuf {
        let path = std::env::temp_dir().join(format!("knixl-pinshim-{}-{tag}", std::process::id()));
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
    fn lookup_ok_parses_commit_and_sha() {
        let r = PinResolver::External(shim("ok", "abc123 sha256:zzz", "", 0));
        let got = r.lookup("htop", "3.2.1").unwrap();
        assert_eq!(got.nixpkgs_rev, "abc123");
        assert_eq!(got.sha256, "sha256:zzz");
    }

    #[test]
    fn lookup_not_found_maps_to_notfound() {
        let r = PinResolver::External(shim("nf", "", "version not found", 1));
        assert!(matches!(r.lookup("htop", "9.9.9"), Err(PinError::NotFound(_))));
    }

    #[test]
    fn lookup_other_failure_maps_to_failed() {
        let r = PinResolver::External(shim("fail", "", "boom", 2));
        assert!(matches!(r.lookup("htop", "3.2.1"), Err(PinError::Failed(_))));
    }

    #[test]
    fn lookup_missing_binary_is_unavailable() {
        let r = PinResolver::External(PathBuf::from("/nonexistent/knixl-no-such-resolver"));
        assert!(matches!(r.lookup("htop", "3.2.1"), Err(PinError::Unavailable(_))));
    }

    #[test]
    fn lookup_malformed_stdout_is_failed() {
        let r = PinResolver::External(shim("bad", "only-one-token", "", 0));
        assert!(matches!(r.lookup("htop", "3.2.1"), Err(PinError::Failed(_))));
    }

    #[test]
    fn lookup_not_found_on_stdout_maps_to_notfound() {
        let r = PinResolver::External(shim("nf-stdout", "version not found", "", 1));
        assert!(matches!(r.lookup("htop", "9.9.9"), Err(PinError::NotFound(_))));
    }

    #[test]
    fn lookup_trailing_tokens_is_failed() {
        let r = PinResolver::External(shim("trailing", "abc123 sha256:zzz extra", "", 0));
        assert!(matches!(r.lookup("htop", "3.2.1"), Err(PinError::Failed(_))));
    }

    const SAMPLE_RESOLVE: &str = r#"{"commit_hash":"5629520edecb69630a3f4d17d3d33fc96c13f6fe","version":"14.1.0","platforms":["x86_64-linux"],"name":"ripgrep"}"#;

    #[test]
    fn commit_hash_reads_the_top_level_field() {
        assert_eq!(
            commit_hash(SAMPLE_RESOLVE).as_deref(),
            Some("5629520edecb69630a3f4d17d3d33fc96c13f6fe")
        );
    }

    #[test]
    fn commit_hash_absent_is_none() {
        assert_eq!(commit_hash(r#"{"version":"14.1.0"}"#), None);
    }

    #[test]
    fn commit_hash_malformed_is_none() {
        assert_eq!(commit_hash("not json"), None);
        assert_eq!(commit_hash(""), None);
    }
}
