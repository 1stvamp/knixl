//! Formatter invocation (pinned nixfmt) and content hashing. Both sit on the
//! reproducibility boundary. SPEC-GRADE SKETCH.

#[derive(Debug, Clone)]
pub struct Formatter {
    pub name: String,     // "nixfmt-rfc-style"
    pub version: String,  // pinned; recorded in the lock
    pub bin: std::path::PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum FormatError {
    #[error("formatter exited non-zero: {0}")]
    NonZero(i32),
    #[error("formatter emitted invalid UTF-8")]
    Utf8,
    #[error("formatter version mismatch: expected {expected}, found {found}")]
    VersionMismatch { expected: String, found: String },
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl Formatter {
    /// Build a formatter, recording the binary's actual reported version (so the lock is
    /// honest). Falls back to `fallback` if the binary cannot be queried. The version token
    /// is the last whitespace-separated word of the first `--version` line (e.g. the
    /// `1.3.1` in `nixfmt 1.3.1`).
    pub fn detect(name: &str, bin: std::path::PathBuf, fallback: &str) -> Formatter {
        let version = std::process::Command::new(&bin)
            .arg("--version")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().last().map(str::to_string))
            })
            .unwrap_or_else(|| fallback.to_string());
        Formatter { name: name.to_string(), version, bin }
    }

    /// Pipe emitted (structurally-correct-but-ugly) Nix through the pinned formatter.
    /// Only the returned, formatted text is ever hashed or written.
    pub fn format(&self, emitted: &str) -> Result<String, FormatError> {
        use std::io::Write;
        use std::process::{Command, Stdio};

        // `-` = format anonymous stdin (bare stdin is deprecated in nixfmt 1.x).
        let mut child = Command::new(&self.bin)
            .arg("-")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        // Write the whole input, then drop stdin to signal EOF before reading stdout.
        // Emitted modules are small (well under a pipe buffer), so this cannot deadlock.
        {
            let mut stdin = child.stdin.take().expect("stdin was piped");
            stdin.write_all(emitted.as_bytes())?;
        }

        let output = child.wait_with_output()?;
        if !output.status.success() {
            return Err(FormatError::NonZero(output.status.code().unwrap_or(-1)));
        }
        String::from_utf8(output.stdout).map_err(|_| FormatError::Utf8)
    }

    /// Verify the on-disk formatter matches the pinned version. Called before generate,
    /// so a formatter drift is caught as a clear error, not a silent output change.
    pub fn verify_version(&self) -> Result<(), FormatError> {
        use std::process::Command;

        let output = Command::new(&self.bin).arg("--version").output()?;
        if !output.status.success() {
            return Err(FormatError::NonZero(output.status.code().unwrap_or(-1)));
        }
        let reported = String::from_utf8_lossy(&output.stdout);
        if reported.contains(&self.version) {
            Ok(())
        } else {
            Err(FormatError::VersionMismatch {
                expected: self.version.clone(),
                found: reported.trim().to_string(),
            })
        }
    }
}

/// blake3 hex, the single hashing function used for inputs and outputs in the lock.
pub fn hash(bytes: &[u8]) -> String {
    let h = blake3::hash(bytes);
    format!("blake3:{}", h.to_hex())
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn formatter_with_bin(bin: &str) -> Formatter {
        Formatter {
            name: "nixfmt-rfc-style".into(),
            version: "0.6.0".into(),
            bin: PathBuf::from(bin),
        }
    }

    #[test]
    fn format_pipes_input_through_the_binary() {
        // `cat` is an identity formatter: it proves the stdin -> stdout plumbing without
        // depending on nixfmt being installed.
        let f = formatter_with_bin("cat");
        assert_eq!(f.format("foo = 1;\n").unwrap(), "foo = 1;\n");
    }

    #[test]
    fn format_reports_non_zero_exit() {
        let f = formatter_with_bin("false");
        assert!(matches!(f.format("x").unwrap_err(), FormatError::NonZero(_)));
    }

    /// Write a throwaway executable that mimics `nixfmt --version` and otherwise cats.
    fn fake_nixfmt(tag: &str, version: &str) -> PathBuf {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        let path = std::env::temp_dir()
            .join(format!("knixl-fake-nixfmt-{}-{tag}", std::process::id()));
        let script = format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo \"nixfmt-rfc-style {version}\"; else cat; fi\n"
        );
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(script.as_bytes()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn verify_version_accepts_matching() {
        let bin = fake_nixfmt("match", "0.6.0");
        let f = Formatter { name: "nixfmt-rfc-style".into(), version: "0.6.0".into(), bin };
        assert!(f.verify_version().is_ok());
    }

    #[test]
    fn verify_version_rejects_mismatch() {
        let bin = fake_nixfmt("mismatch", "0.5.0");
        let f = Formatter { name: "nixfmt-rfc-style".into(), version: "0.6.0".into(), bin };
        assert!(matches!(f.verify_version().unwrap_err(), FormatError::VersionMismatch { .. }));
    }

    #[test]
    fn hash_is_blake3_prefixed_and_stable() {
        assert_eq!(hash(b"abc"), hash(b"abc"));
        assert!(hash(b"abc").starts_with("blake3:"));
    }
}
