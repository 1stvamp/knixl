//! Fetches a declarative module's `knixl-module.kdl` manifest from a git source at a pinned
//! rev, plus the cache-path and hash helpers that let a caller store and verify it. Mirrors
//! `ModuleResolver` in `module.rs`: shells out to `git`, no libgit2 dependency.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Errors from fetching a module manifest.
#[derive(Debug, thiserror::Error)]
pub enum ModuleFetchError {
    #[error("git is not available: {0}")]
    Unavailable(String),
    #[error("failed to fetch {url}@{rev}: {reason}")]
    Failed {
        url: String,
        rev: String,
        reason: String,
    },
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// `$XDG_CACHE_HOME/knixl` (falling back to `$HOME/.cache/knixl`). Duplicated from
/// `knixl-oracle`'s `cache_dir()` rather than shared: the crate graph in CLAUDE.md is
/// `knixl-nix` -> nothing, `knixl-oracle` standing alone, and this is a two-line env lookup,
/// not worth a new cross-dependency.
fn cache_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("knixl"))
}

/// Deterministic cache location for a fetched module manifest: `module-<hex>.kdl` under the
/// knixl cache dir, keyed on `blake3(url\nrev\npath)` so two different sources (even ones that
/// share a rev, or both use an empty `path`) never collide. Returns `None` when no cache/home
/// directory can be determined, mirroring `knixl-oracle::cache_path`.
pub fn module_cache_path(url: &str, rev: &str, path: &str) -> Option<PathBuf> {
    let key = format!("{url}\n{rev}\n{path}");
    let hash = crate::hash(key.as_bytes());
    let hex = hash.strip_prefix("blake3:").unwrap_or(&hash);
    Some(cache_dir()?.join(format!("module-{hex}.kdl")))
}

/// The blake3 hash of a fetched module's manifest text, in the same `"blake3:<hex>"` form the
/// rest of the lock uses.
pub fn hash_module(text: &str) -> String {
    crate::hash(text.as_bytes())
}

/// The manifest's path inside the source tree: `knixl-module.kdl` at the repo root when `path`
/// is empty, else `<path>/knixl-module.kdl`.
fn manifest_relpath(path: &str) -> String {
    if path.is_empty() {
        "knixl-module.kdl".to_string()
    } else {
        format!("{}/knixl-module.kdl", path.trim_end_matches('/'))
    }
}

/// Fetch `<path>/knixl-module.kdl` at `rev` from `url` and return its text. Writes nothing to
/// the knixl cache; the caller decides whether and where to persist it via
/// `module_cache_path`.
///
/// Does a shallow fetch of the single rev into a throwaway temp dir, then reads the manifest
/// straight out of git's object store (`git show FETCH_HEAD:<relpath>`) rather than checking
/// out a working tree: cheaper, and it never leaves a partial checkout behind on error.
pub fn fetch_module(url: &str, rev: &str, path: &str) -> Result<String, ModuleFetchError> {
    let relpath = manifest_relpath(path);
    let tmp = unique_temp_dir();
    std::fs::create_dir_all(&tmp)?;
    let result = fetch_module_into(&tmp, url, rev, &relpath);
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

fn fetch_module_into(
    tmp: &Path,
    url: &str,
    rev: &str,
    relpath: &str,
) -> Result<String, ModuleFetchError> {
    run_git(tmp, &["init", "-q"], url, rev)?;
    run_git(tmp, &["remote", "add", "origin", url], url, rev)?;
    run_git(
        tmp,
        &["fetch", "-q", "--depth", "1", "origin", rev],
        url,
        rev,
    )?;
    let out = crate::output_retrying_etxtbsy(|| {
        let mut c = Command::new("git");
        c.arg("-C").arg(tmp);
        c.args(["show", &format!("FETCH_HEAD:{relpath}")]);
        c
    })
    .map_err(|e| to_fetch_error(e, url, rev))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(ModuleFetchError::Failed {
            url: url.to_string(),
            rev: rev.to_string(),
            reason: if stderr.is_empty() {
                format!("{relpath} not found at {rev}")
            } else {
                stderr
            },
        });
    }
    String::from_utf8(out.stdout).map_err(|_| ModuleFetchError::Failed {
        url: url.to_string(),
        rev: rev.to_string(),
        reason: format!("{relpath} is not valid UTF-8"),
    })
}

/// Run one step of the fetch (`init`, `remote add`, `fetch`) in `dir`, mapping a spawn failure
/// or non-zero exit to a `ModuleFetchError` that carries the url and rev being fetched.
fn run_git(dir: &Path, args: &[&str], url: &str, rev: &str) -> Result<(), ModuleFetchError> {
    let out = crate::output_retrying_etxtbsy(|| {
        let mut c = Command::new("git");
        c.arg("-C").arg(dir);
        c.args(args);
        c
    })
    .map_err(|e| to_fetch_error(e, url, rev))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(ModuleFetchError::Failed {
            url: url.to_string(),
            rev: rev.to_string(),
            reason: stderr,
        });
    }
    Ok(())
}

fn to_fetch_error(e: std::io::Error, url: &str, rev: &str) -> ModuleFetchError {
    if e.kind() == std::io::ErrorKind::NotFound {
        ModuleFetchError::Unavailable("git not found".to_string())
    } else {
        ModuleFetchError::Failed {
            url: url.to_string(),
            rev: rev.to_string(),
            reason: e.to_string(),
        }
    }
}

/// A fresh, never-reused temp dir for one fetch's throwaway git checkout. Keyed on pid, a
/// per-process counter, and the current time, so concurrent fetches (even within the same
/// process) never collide.
fn unique_temp_dir() -> PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "knixl-modulefetch-{}-{nanos}-{n}",
        std::process::id()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_path_is_stable_and_source_keyed() {
        let dir = std::env::temp_dir().join(format!(
            "knixl-modulefetch-cachetest-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("XDG_CACHE_HOME", &dir);

        let a = module_cache_path("https://example.com/repo", "abc123", "").unwrap();
        let b = module_cache_path("https://example.com/repo", "abc123", "").unwrap();
        assert_eq!(a, b, "same (url, rev, path) must hash to the same path");
        assert!(
            a.starts_with(dir.join("knixl")),
            "cache path must land under the knixl cache dir: {a:?}"
        );
        assert_eq!(a.extension().and_then(|e| e.to_str()), Some("kdl"));

        let different_rev = module_cache_path("https://example.com/repo", "def456", "").unwrap();
        assert_ne!(a, different_rev, "a different rev must not collide");

        let different_path =
            module_cache_path("https://example.com/repo", "abc123", "sub/dir").unwrap();
        assert_ne!(a, different_path, "a different path must not collide");

        let different_url = module_cache_path("https://example.com/other", "abc123", "").unwrap();
        assert_ne!(a, different_url, "a different url must not collide");

        std::env::remove_var("XDG_CACHE_HOME");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hash_module_is_stable_and_blake3_prefixed() {
        assert_eq!(hash_module("foo"), hash_module("foo"));
        assert!(hash_module("foo").starts_with("blake3:"));
        assert_ne!(hash_module("foo"), hash_module("bar"));
    }

    #[test]
    fn manifest_relpath_is_root_or_subdir() {
        assert_eq!(manifest_relpath(""), "knixl-module.kdl");
        assert_eq!(
            manifest_relpath("modules/disko"),
            "modules/disko/knixl-module.kdl"
        );
        assert_eq!(
            manifest_relpath("modules/disko/"),
            "modules/disko/knixl-module.kdl"
        );
    }

    /// A local `file://` remote exercises the real git path (init, remote add, shallow
    /// fetch, `show FETCH_HEAD:...`) entirely offline. Skipped, not failed, when `git` is not
    /// on PATH, matching the formatter-gated golden tests elsewhere in the workspace.
    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Build a local repo at `<tmp>/repo` with `knixl-module.kdl` at `manifest_path` (a
    /// relative path, possibly nested, empty meaning repo root), commit it, and return the
    /// repo's `file://` URL and the commit's rev.
    fn local_module_repo(tag: &str, manifest_path: &str, contents: &str) -> (PathBuf, String) {
        let repo = std::env::temp_dir().join(format!(
            "knixl-modulefetch-repo-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&repo);
        std::fs::create_dir_all(&repo).unwrap();

        let run = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };

        run(&["init", "-q"]);
        let manifest_dir = if manifest_path.is_empty() {
            repo.clone()
        } else {
            repo.join(manifest_path)
        };
        std::fs::create_dir_all(&manifest_dir).unwrap();
        std::fs::write(manifest_dir.join("knixl-module.kdl"), contents).unwrap();
        run(&["add", "."]);
        run(&[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "module",
        ]);

        let out = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        assert!(out.status.success());
        let rev = String::from_utf8(out.stdout).unwrap().trim().to_string();

        (repo, rev)
    }

    #[test]
    fn fetch_module_reads_the_manifest_at_the_repo_root() {
        if !git_available() {
            eprintln!("skipping fetch_module_reads_the_manifest_at_the_repo_root: git not found");
            return;
        }
        let contents = "module \"disko\" {\n}\n";
        let (repo, rev) = local_module_repo("root", "", contents);
        let url = format!("file://{}", repo.display());

        let got = fetch_module(&url, &rev, "").unwrap();
        assert_eq!(got, contents);

        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn fetch_module_reads_the_manifest_from_a_subdirectory() {
        if !git_available() {
            eprintln!(
                "skipping fetch_module_reads_the_manifest_from_a_subdirectory: git not found"
            );
            return;
        }
        let contents = "module \"sops-nix\" {\n}\n";
        let (repo, rev) = local_module_repo("subdir", "modules/sops-nix", contents);
        let url = format!("file://{}", repo.display());

        let got = fetch_module(&url, &rev, "modules/sops-nix").unwrap();
        assert_eq!(got, contents);

        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn fetch_module_missing_manifest_is_a_clear_error() {
        if !git_available() {
            eprintln!("skipping fetch_module_missing_manifest_is_a_clear_error: git not found");
            return;
        }
        // A commit with no knixl-module.kdl at all: `git show` fails with a "does not exist"
        // error, which must surface as `ModuleFetchError::Failed`, not a panic or a bare Ok.
        let (repo, rev) = local_module_repo("missing", "unused-marker-dir", "irrelevant");
        let url = format!("file://{}", repo.display());

        let err = fetch_module(&url, &rev, "somewhere/else").unwrap_err();
        assert!(matches!(err, ModuleFetchError::Failed { .. }));

        let _ = std::fs::remove_dir_all(&repo);
    }
}
