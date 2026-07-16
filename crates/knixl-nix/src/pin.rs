//! Version-to-commit resolution for `knixl install pkg@version`. The resolver is an injected
//! command (`KNIXL_PIN_RESOLVER`, default `knixl-pin-resolve`) mapping `name version` to a
//! nixpkgs commit and its sha256, run only at pin time. A missing resolver is Unavailable
//! (blocks the pin), never a wrong result.

use std::path::PathBuf;
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

/// A handle to the version resolver. `KNIXL_PIN_RESOLVER` overrides the binary (a shim in
/// tests); the default is the bundled `knixl-pin-resolve`.
#[derive(Debug, Clone)]
pub struct PinResolver {
    pub bin: PathBuf,
}

impl PinResolver {
    pub fn resolve() -> PinResolver {
        let bin = std::env::var_os("KNIXL_PIN_RESOLVER")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("knixl-pin-resolve"));
        PinResolver { bin }
    }

    /// Resolve `pkgs.<name>` at `version` to a nixpkgs commit and its sha256.
    pub fn lookup(&self, name: &str, version: &str) -> Result<Resolved, PinError> {
        let out = crate::output_retrying_etxtbsy(|| {
            let mut c = Command::new(&self.bin);
            c.args([name, version]);
            c
        })
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                PinError::Unavailable(format!("{} not found", self.bin.display()))
            } else {
                PinError::Unavailable(e.to_string())
            }
        })?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let stdout = String::from_utf8_lossy(&out.stdout);
            let combined = format!("{}{}", stderr, stdout).trim().to_string();
            let combined_lower = combined.to_lowercase();
            if combined_lower.contains("not found") {
                return Err(PinError::NotFound(format!("{name} {version}: {combined}")));
            }
            let err_msg = if !stderr.is_empty() {
                stderr.trim().to_string()
            } else {
                stdout.trim().to_string()
            };
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
        let r = PinResolver { bin: shim("ok", "abc123 sha256:zzz", "", 0) };
        let got = r.lookup("htop", "3.2.1").unwrap();
        assert_eq!(got.nixpkgs_rev, "abc123");
        assert_eq!(got.sha256, "sha256:zzz");
    }

    #[test]
    fn lookup_not_found_maps_to_notfound() {
        let r = PinResolver { bin: shim("nf", "", "version not found", 1) };
        assert!(matches!(r.lookup("htop", "9.9.9"), Err(PinError::NotFound(_))));
    }

    #[test]
    fn lookup_other_failure_maps_to_failed() {
        let r = PinResolver { bin: shim("fail", "", "boom", 2) };
        assert!(matches!(r.lookup("htop", "3.2.1"), Err(PinError::Failed(_))));
    }

    #[test]
    fn lookup_missing_binary_is_unavailable() {
        let r = PinResolver { bin: PathBuf::from("/nonexistent/knixl-no-such-resolver") };
        assert!(matches!(r.lookup("htop", "3.2.1"), Err(PinError::Unavailable(_))));
    }

    #[test]
    fn lookup_malformed_stdout_is_failed() {
        let r = PinResolver { bin: shim("bad", "only-one-token", "", 0) };
        assert!(matches!(r.lookup("htop", "3.2.1"), Err(PinError::Failed(_))));
    }

    #[test]
    fn lookup_not_found_on_stdout_maps_to_notfound() {
        let r = PinResolver { bin: shim("nf-stdout", "version not found", "", 1) };
        assert!(matches!(r.lookup("htop", "9.9.9"), Err(PinError::NotFound(_))));
    }

    #[test]
    fn lookup_trailing_tokens_is_failed() {
        let r = PinResolver { bin: shim("trailing", "abc123 sha256:zzz extra", "", 0) };
        assert!(matches!(r.lookup("htop", "3.2.1"), Err(PinError::Failed(_))));
    }
}
