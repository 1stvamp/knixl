# Built-in default version resolver Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `knixl install pkg@version` work out of the box by shipping a built-in default version resolver (nixhub/devbox resolve API over HTTPS + `nix-prefetch-url`), keeping `KNIXL_PIN_RESOLVER` as the override.

**Architecture:** `PinResolver` becomes `External(path) | Builtin`. `Builtin` GETs `https://search.devbox.sh/v1/resolve?name=..&version=..` with `ureq` (proxy-aware, rustls, blocking), reads the top-level `commit_hash` via `serde_json`, and prefetches the sha256 with `nix-prefetch-url`. The `External` path is unchanged.

**Tech Stack:** Rust (`knixl-nix`), `ureq` 3 (new dep), `serde_json` (already in the lock).

## Global Constraints

- British spelling in comments; no em-dashes or en-dashes.
- MSRV 1.87.
- New dependency `ureq` (v3, default features: rustls + gzip) is authorised for this work; `serde_json` is already a workspace dependency (knixl-oracle).
- The resolver runs only at pin time; a failure to resolve is a hard stop upstream (never a wrong pin). Error mapping matches the existing `External` semantics: `Unavailable` (index unreachable / tool missing), `NotFound` (no such version), `Failed` (bad response / prefetch failure).
- Commit only when tests pass. Use GitButler: `but commit feat/version-pinning -c -m "<msg>" --changes <ids>` (ids from `but status`). Never raw git. This lands on `feat/version-pinning` so slice D ships usable.

---

### Task 1: Built-in resolver (knixl-nix)

**Files:**
- Modify: `crates/knixl-nix/Cargo.toml` (add `ureq`, `serde_json`)
- Modify: `crates/knixl-nix/src/pin.rs` (enum refactor, `commit_hash`, builtin lookup, update tests)

**Interfaces:**
- Produces: `pub enum PinResolver { External(PathBuf), Builtin }`; `resolve()`/`lookup()` unchanged signatures; `Resolved`/`PinError` unchanged.

- [ ] **Step 1: Add dependencies**

In `crates/knixl-nix/Cargo.toml` `[dependencies]`, add:
```toml
ureq = "3"
serde_json = { workspace = true }
```
Check the root `Cargo.toml` `[workspace.dependencies]`: if `serde_json` is defined there, use `{ workspace = true }` (as above); if not, match the exact version knixl-oracle depends on (`grep serde_json crates/knixl-oracle/Cargo.toml`) and use that literal version. `ureq = "3"` uses default features (rustls + gzip); do not add `default-features = false`.

- [ ] **Step 2: Write the failing `commit_hash` tests**

Add to the `tests` module in `crates/knixl-nix/src/pin.rs` (the sample is a trimmed real response from `search.devbox.sh/v1/resolve`):

```rust
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
```

- [ ] **Step 3: Run to verify they fail**

Run: `cargo test -p knixl-nix commit_hash 2>&1 | tail`
Expected: FAIL to compile (`commit_hash` does not exist).

- [ ] **Step 4: Refactor `PinResolver` to an enum and add the builtin path**

Replace the `PinResolver` struct, `resolve`, and `lookup` in `crates/knixl-nix/src/pin.rs` with the enum form. Keep `Resolved` and `PinError` exactly as they are. Change the top `use` to `use std::path::{Path, PathBuf};` and add `use std::io::Read;` (for ureq's body read).

```rust
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
```

- [ ] **Step 5: Update the existing shim tests to the enum**

In the `tests` module, every `PinResolver { bin: X }` becomes `PinResolver::External(X)`. The seven existing tests (`lookup_ok_parses_commit_and_sha`, `lookup_not_found_maps_to_notfound`, `lookup_other_failure_maps_to_failed`, `lookup_missing_binary_is_unavailable`, `lookup_malformed_stdout_is_failed`, `lookup_not_found_on_stdout_maps_to_notfound`, `lookup_trailing_tokens_is_failed`) keep their assertions; only the constructor changes. Example:
```rust
let r = PinResolver::External(shim("ok", "abc123 sha256:zzz", "", 0));
```
Do NOT add a test that calls `lookup_builtin`/`Builtin` (it hits the network / nix); the builtin path's pure part is covered by the `commit_hash` tests, and the shell-out is untested glue.

- [ ] **Step 6: Run tests + clippy + build**

Run: `cargo test -p knixl-nix 2>&1 | tail` (all pass: the 7 external shim tests + 3 new commit_hash tests, plus the crate's others)
Run: `cargo build --workspace 2>&1 | tail` (the CLI's `PinResolver::resolve()` still compiles: `resolve()`/`lookup()` are unchanged)
Run: `cargo clippy -p knixl-nix 2>&1 | grep -cE 'warning:|error'` (expect 0)

Note: the CLI calls `PinResolver::resolve()` and `.lookup(..)` (unchanged), so no CLI edit is needed. Confirm `Cargo.lock` gained `ureq` and its rustls stack (expected).

- [ ] **Step 7: Commit**

`but commit feat/version-pinning -c -m "feat(nix): built-in default version resolver (nixhub via ureq)" --changes <ids>`

---

### Task 2: docs + close #21 + verify

**Files:**
- Modify: `docs/05-cli.md`

- [ ] **Step 1: Document the default**

In the `knixl install` bullet (which already documents `pkg@version`), add that resolution works out of the box by default: it queries the nixhub/devbox version index over HTTPS (honouring `HTTP_PROXY`/`HTTPS_PROXY`/`NO_PROXY`) and prefetches the sha with `nix` (so `nix` must be present); `KNIXL_PIN_RESOLVER` overrides it with an external `<name> <version>` -> `<commit> <sha256>` command. British spelling, no dashes.

- [ ] **Step 2: Full workspace verify**

Run: `cargo test --workspace 2>&1 | grep -cE 'FAILED'` (expect 0; if a single knixl-nix ETXTBSY shim test flakes under parallelism, re-run `cargo test -p knixl-nix` once to confirm)
Run: `cargo clippy --workspace --all-targets 2>&1 | grep -cE 'warning:|error'` (expect 0)

- [ ] **Step 3: Commit (closes #21)**

`but commit feat/version-pinning -c -m "docs(cli): note the built-in default version resolver

Closes #21." --changes <ids>`

---

## Self-review notes

- Spec coverage: enum refactor + builtin (Task 1), deps (Task 1 Step 1), pure `commit_hash` tests (Task 1 Step 2), external path unchanged + tests updated (Task 1 Step 5), docs + close #21 (Task 2). All covered.
- Error mapping matches the spec: 404 -> NotFound; other HTTP / transport / tool-missing -> Unavailable; missing commit_hash / prefetch failure -> Failed.
- `resolve()`/`lookup()` signatures are unchanged, so the CLI (`PinResolver::resolve().lookup(..)`) needs no edit.
- No determinism concern (resolution is pin-time only; the lock stores the result). ureq default features keep rustls (no system OpenSSL).
