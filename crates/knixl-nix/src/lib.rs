//! Formatter invocation (pinned nixfmt) and content hashing. Both sit on the
//! reproducibility boundary. SPEC-GRADE SKETCH.

pub mod nixeval;
pub mod pin;

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
    #[error("formatter `{0}` not found; install nixfmt-rfc-style or set KNIXL_FORMATTER")]
    NotFound(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl FormatError {
    /// Turn a spawn/exec io error into a clear `NotFound` when the binary is missing.
    fn from_spawn(bin: &std::path::Path, e: std::io::Error) -> Self {
        if e.kind() == std::io::ErrorKind::NotFound {
            FormatError::NotFound(bin.display().to_string())
        } else {
            FormatError::Io(e)
        }
    }
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
            .spawn()
            .map_err(|e| FormatError::from_spawn(&self.bin, e))?;

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

        let output = output_retrying_etxtbsy(|| {
            let mut c = Command::new(&self.bin);
            c.arg("--version");
            c
        })
        .map_err(|e| FormatError::from_spawn(&self.bin, e))?;
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

/// Run a command to completion, retrying briefly on `ETXTBSY`. Spawning a binary that was
/// only just written can race with another thread's fork holding a write handle to it (a
/// well-known issue for multithreaded programs that exec freshly-created executables); a
/// few short retries let that window close.
pub(crate) fn output_retrying_etxtbsy(
    mut build: impl FnMut() -> std::process::Command,
) -> std::io::Result<std::process::Output> {
    let mut attempt = 0;
    loop {
        match build().output() {
            Err(e) if e.raw_os_error() == Some(26) && attempt < 10 => {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            other => return other,
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
    fn missing_formatter_is_a_clear_error_not_a_bare_io_error() {
        let f = formatter_with_bin("knixl-definitely-no-such-formatter-xyz");
        match f.format("{ }\n").unwrap_err() {
            FormatError::NotFound(name) => assert!(name.contains("knixl-definitely-no-such-formatter-xyz")),
            other => panic!("expected NotFound, got {other:?}"),
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
        file.flush().unwrap();
        drop(file); // close the write handle before exec, or spawning races with ETXTBSY
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
