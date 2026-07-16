# install pkg@version: per-host version pinning Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let `knixl install pkg@version` pin a package version per host: KDL holds the version intent, the lock records the resolved nixpkgs commit + sha256 per host, and the generator emits that package from a pinned import mixed into the host baseline.

**Architecture:** Historical-commit mixing (ADR 0005). Resolution (version to commit+sha) is an injected command run only at install/upgrade; the result is locked so generate/check stay offline and pure. The pinned package emits as `(import (fetchTarball { url; sha256; }) {}).<name>`, deduped by the existing let-hoisting pass.

**Tech Stack:** Rust workspace (knixl-ir, knixl-modules, knixl-lock, knixl-nix, knixl-pipeline, knixl-cli), bubbletea-rs + bubbletea-widgets + lipgloss (TUI), clap (CLI).

## Global Constraints

- British spelling in prose/comments; no em-dashes or en-dashes (colons, parentheses, commas, full stops only).
- Banned vocabulary (docs, comments, commit messages): passionate, leverage, robust, seamless, delve, and the AI-smell set.
- Deterministic emit: no `HashMap` in emit paths; pins emit in stable order; `fetchTarball` url/sha are byte-stable from the lock. Generate twice must be byte-identical.
- Dependency direction is fixed: `knixl-modules` depends on `knixl-ir` + `knixl-oracle` ONLY. It must NOT import `knixl-lock`. The pin view threaded into `LowerCtx` is a modules-owned type; the pipeline maps `knixl-lock::Pin` into it.
- MSRV 1.87; kdl 6.5 pin unchanged; do not add Rust dependencies.
- TUI-first: interactive CLI flows default to the TUI; the plain/text path is used only under `--yes` or a non-TTY (CI/pipes).
- Commit only when a task's tests pass. Use GitButler: `but commit feat/version-pinning -c -m "<msg>" --changes <ids>` (ids from `but status`). Branch `feat/version-pinning` already exists (holds the ADR + spec commit). Never raw git.
- Injection pattern: nix/network work is injected behind a command resolved from an env var (mirror `KNIXL_NIX` in `crates/knixl-nix/src/nixeval.rs`), so tests use a shim and offline degrades to a clear error, never a wrong result.

---

### Task 1: Lock schema for per-host pins (knixl-lock)

**Files:**
- Modify: `crates/knixl-lock/src/model.rs` (add `Pin`, `Lock.pins`, parse `host`/`pin`, render, tests)

**Interfaces:**
- Produces:
  - `pub struct Pin { pub package: String, pub version: String, pub nixpkgs_rev: String, pub sha256: String }` (Debug, Clone, PartialEq, Eq)
  - `Lock` gains `pub pins: BTreeMap<String /*host*/, Vec<Pin>>` (host key sorted by BTreeMap; each Vec sorted by package for determinism)

- [ ] **Step 1: Write the failing round-trip test**

Add to the tests in `crates/knixl-lock/src/model.rs` (match the file's existing test style):

```rust
#[test]
fn pins_round_trip_and_are_deterministic() {
    let src = r#"lock version=1 {
    tool version="0.3.1"
    formatter name="nixfmt-rfc-style" version="0.6.0"
    oracle nixpkgs-rev="deadbeef" options-hash="blake3:x"
    host "laptop" {
        pin "htop" version="3.2.1" nixpkgs-rev="abc123" sha256="sha256:zzz"
    }
}
"#;
    let lock = Lock::parse(src).expect("parse");
    let pins = lock.pins.get("laptop").expect("laptop pins");
    assert_eq!(pins.len(), 1);
    assert_eq!(pins[0].package, "htop");
    assert_eq!(pins[0].version, "3.2.1");
    assert_eq!(pins[0].nixpkgs_rev, "abc123");
    assert_eq!(pins[0].sha256, "sha256:zzz");
    // Re-parsing the rendered form yields the same pins (byte-stable ordering).
    let again = Lock::parse(&lock.render()).expect("reparse");
    assert_eq!(again.pins, lock.pins);
}

#[test]
fn lock_without_host_block_parses_with_no_pins() {
    let src = r#"lock version=1 {
    tool version="0.3.1"
    formatter name="nixfmt-rfc-style" version="0.6.0"
    oracle nixpkgs-rev="deadbeef" options-hash="blake3:x"
}
"#;
    let lock = Lock::parse(src).expect("parse");
    assert!(lock.pins.is_empty(), "back-compat: no host block means no pins");
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p knixl-lock pins_ 2>&1 | tail`
Expected: FAIL to compile (`Pin`, `lock.pins` do not exist).

- [ ] **Step 3: Add the struct, the field, parsing, and rendering**

Add the struct near `OutputEntry`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    pub package: String,
    pub version: String,
    pub nixpkgs_rev: String,
    pub sha256: String,
}
```

Add `pub pins: BTreeMap<String, Vec<Pin>>` to `Lock` (after `outputs`). In `parse`, add `let mut pins: BTreeMap<String, Vec<Pin>> = BTreeMap::new();` with the other accumulators, add a `"host"` arm to the node match, and include `pins` in the returned `Lock`:

```rust
                "host" => {
                    let host = arg_str(node, 0)?;
                    let mut list = Vec::new();
                    if let Some(body) = node.children() {
                        for p in body.nodes() {
                            if p.name().value() != "pin" {
                                return Err(LockError::Malformed(format!(
                                    "unexpected `{}` in host block (expected `pin`)",
                                    p.name().value()
                                )));
                            }
                            list.push(Pin {
                                package: arg_str(p, 0)?,
                                version: prop_str(p, "version")?,
                                nixpkgs_rev: prop_str(p, "nixpkgs-rev")?,
                                sha256: prop_str(p, "sha256")?,
                            });
                        }
                    }
                    list.sort_by(|a, b| a.package.cmp(&b.package));
                    pins.insert(host, list);
                }
```

In `render`, after the `module` loop and before/after `outputs` (pick a stable position: after modules), emit each host block in BTreeMap (sorted) order, pins already sorted:

```rust
        for (host, list) in &self.pins {
            if list.is_empty() { continue; }
            s.push('\n');
            s.push_str(&format!("    host \"{}\" {{\n", esc(host)));
            for p in list {
                s.push_str(&format!(
                    "        pin \"{}\" version=\"{}\" nixpkgs-rev=\"{}\" sha256=\"{}\"\n",
                    esc(&p.package), esc(&p.version), esc(&p.nixpkgs_rev), esc(&p.sha256),
                ));
            }
            s.push_str("    }\n");
        }
```

Add `pins` to every other `Lock { ... }` construction in the workspace (search `Lock {` in `crates/knixl-pipeline/src/gather.rs` — the fresh-lock seed there needs `pins: BTreeMap::new()`).

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test -p knixl-lock 2>&1 | tail` (all pass, incl. the 2 new)
Run: `cargo build --workspace 2>&1 | tail` (the `gather.rs` fresh-lock seed compiles with the new field)
Then `cargo clippy -p knixl-lock` clean.

- [ ] **Step 5: Commit**

`but commit feat/version-pinning -c -m "feat(lock): per-host package pins in the lockfile" --changes <ids>`

---

### Task 2: Injected version resolver (knixl-nix)

**Files:**
- Create: `crates/knixl-nix/src/pin.rs`
- Modify: `crates/knixl-nix/src/lib.rs` (add `pub mod pin;`)

**Interfaces:**
- Produces:
  - `pub struct PinResolver { pub bin: PathBuf }`
  - `pub struct Resolved { pub nixpkgs_rev: String, pub sha256: String }`
  - `pub enum PinError { Unavailable(String), NotFound(String), Failed(String) }` (thiserror, like `NixError`)
  - `impl PinResolver { pub fn resolve() -> PinResolver; pub fn lookup(&self, name: &str, version: &str) -> Result<Resolved, PinError>; }`
- Consumes: `crate::output_retrying_etxtbsy` (existing).

Protocol: `lookup` runs `<bin> <name> <version>`. Exit 0 with stdout `"<commit> <sha256>"` (whitespace-separated, one line) => `Resolved`. Exit non-zero whose stderr/stdout contains `not found` (case-insensitive) => `NotFound`. Other non-zero => `Failed(stderr)`. Spawn failure (NotFound errno) => `Unavailable`. Malformed stdout on exit 0 => `Failed`.

- [ ] **Step 1: Write the failing tests**

Create `crates/knixl-nix/src/pin.rs` with a shim-based test module mirroring `nixeval.rs`'s shim pattern (a `#!/bin/sh` script written to a temp file, `chmod 0755`, closed before exec):

```rust
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
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p knixl-nix lookup_ 2>&1 | tail`
Expected: FAIL to compile (`pin` module does not exist).

- [ ] **Step 3: Implement `pin.rs`**

```rust
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
            let err = String::from_utf8_lossy(&out.stderr);
            let msg = err.trim().to_string();
            if err.to_lowercase().contains("not found") {
                return Err(PinError::NotFound(format!("{name} {version}: {msg}")));
            }
            return Err(PinError::Failed(msg));
        }
        let line = String::from_utf8_lossy(&out.stdout);
        let mut it = line.split_whitespace();
        match (it.next(), it.next()) {
            (Some(rev), Some(sha)) => {
                Ok(Resolved { nixpkgs_rev: rev.to_string(), sha256: sha.to_string() })
            }
            _ => Err(PinError::Failed(format!("resolver output not `<commit> <sha256>`: {}", line.trim()))),
        }
    }
}
```

Add `pub mod pin;` to `crates/knixl-nix/src/lib.rs` (next to the existing module declarations).

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test -p knixl-nix 2>&1 | tail` (all pass incl. 5 new). `cargo clippy -p knixl-nix` clean.

- [ ] **Step 5: Commit**

`but commit feat/version-pinning -c -m "feat(nix): injected version resolver (KNIXL_PIN_RESOLVER)" --changes <ids>`

---

### Task 3: package version emit + pin threading (knixl-modules + knixl-pipeline)

**Files:**
- Modify: `crates/knixl-modules/src/lib.rs` (add `ResolvedPin`, `LowerCtx.pins`, `LowerCtx::new` param, `ctx.pin`)
- Modify: `crates/knixl-modules/src/builtin/package.rs` (schema `version` prop, versioned emit, tests)
- Modify: `crates/knixl-pipeline/src/lib.rs` (map lock pins into `LowerCtx`)
- Modify: `crates/knixl-pipeline/src/gather.rs` (pass `lock.pins` to `generate`)

**Interfaces:**
- Consumes: `knixl_lock::Pin` (pipeline only), `NixExpr::{Apply, Select, AttrSet, Ref, Str}`, `AttrKey::Ident` (knixl-ir).
- Produces:
  - knixl-modules: `pub struct ResolvedPin { pub package: String, pub version: String, pub nixpkgs_rev: String, pub sha256: String }`; `LowerCtx::new(scope, registry, diags, pins: Vec<ResolvedPin>)`; `pub fn pin(&self, package: &str, version: &str) -> Option<&ResolvedPin>`.

Dependency rule: `ResolvedPin` is defined in `knixl-modules` (NOT `knixl-lock`). The pipeline maps `knixl_lock::Pin` -> `ResolvedPin` when building the `LowerCtx`.

- [ ] **Step 1: Write the failing emit test**

Add to `crates/knixl-modules/src/builtin/package.rs` tests. The `LowerCtx::new` gains a pins arg (Step 3 updates the existing test's `LowerCtx::new` call too).

```rust
#[test]
fn versioned_package_lowers_to_a_pinned_import_select() {
    let m = PackageModule::new();
    let n = node("package \"htop\" version=\"3.2.1\"");
    let reg = Registry::new();
    let mut diags = Vec::new();
    let pins = vec![crate::ResolvedPin {
        package: "htop".into(),
        version: "3.2.1".into(),
        nixpkgs_rev: "abc123".into(),
        sha256: "sha256:zzz".into(),
    }];
    let mut ctx = LowerCtx::new(Scope { host: "web".into() }, &reg, &mut diags, pins);

    let out = m.lower(&n, &mut ctx).unwrap();
    // The emitted text must contain the pinned fetchTarball import and select the package
    // from it, not from baseline `pkgs`.
    let a = &out.units[0].assignment;
    let rendered = format!("{:?}", a.value); // structural check below is the real assertion
    assert!(rendered.contains("abc123"), "carries the pinned commit: {rendered}");
    assert!(rendered.contains("htop"), "selects the package: {rendered}");
    assert!(rendered.contains("fetchTarball"), "uses fetchTarball: {rendered}");
}

#[test]
fn versioned_package_without_a_matching_pin_is_a_lower_error() {
    let m = PackageModule::new();
    let n = node("package \"htop\" version=\"3.2.1\"");
    let reg = Registry::new();
    let mut diags = Vec::new();
    let mut ctx = LowerCtx::new(Scope { host: "web".into() }, &reg, &mut diags, vec![]);
    assert!(m.lower(&n, &mut ctx).is_err(), "declared version with no lock pin is an error");
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p knixl-modules versioned_package 2>&1 | tail`
Expected: FAIL to compile (`ResolvedPin`, 4-arg `LowerCtx::new`, `version` prop unknown).

- [ ] **Step 3: Add `ResolvedPin` + thread pins through `LowerCtx`**

In `crates/knixl-modules/src/lib.rs`, add the struct (near `Scope`):

```rust
/// A resolved package pin, threaded into lowering so the `package` module can emit a pinned
/// import. Mapped from the lock by the pipeline (knixl-modules must not depend on knixl-lock).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPin {
    pub package: String,
    pub version: String,
    pub nixpkgs_rev: String,
    pub sha256: String,
}
```

Extend `LowerCtx`:

```rust
pub struct LowerCtx<'a> {
    scope: Scope,
    registry: &'a Registry,
    diags: &'a mut Vec<Diagnostic>,
    pins: Vec<ResolvedPin>,
}

impl<'a> LowerCtx<'a> {
    pub fn new(
        scope: Scope,
        registry: &'a Registry,
        diags: &'a mut Vec<Diagnostic>,
        pins: Vec<ResolvedPin>,
    ) -> Self {
        Self { scope, registry, diags, pins }
    }
    pub fn scope(&self) -> &Scope { &self.scope }
    /// The resolved pin for a package at a version on this host, if any.
    pub fn pin(&self, package: &str, version: &str) -> Option<&ResolvedPin> {
        self.pins.iter().find(|p| p.package == package && p.version == version)
    }
    // ... existing methods unchanged ...
}
```

Update EVERY `LowerCtx::new(...)` call site to pass a pins vec: all module unit tests across `crates/knixl-modules/src/**` that call `LowerCtx::new(Scope{..}, &reg, &mut diags)` gain a fourth arg `vec![]` (search the crate for `LowerCtx::new(`).

- [ ] **Step 4: Add the `version` prop and versioned emit to the package module**

In `crates/knixl-modules/src/builtin/package.rs`, add the optional prop to `schema()`:

```rust
        props: vec![Field {
            name: "version".into(),
            ty: ValueTy::Str,
            required: false,
            doc: "Pin to this version, resolved to a nixpkgs commit at install time.".into(),
        }],
```

Replace `lower` so a `version` prop emits from a pinned import. Build the expression `(import (fetchTarball { url = "..."; sha256 = "..."; }) {}).<name>` structurally so the let-hoisting pass can dedup repeated identical imports:

```rust
    fn lower(&self, node: &KdlNode, ctx: &mut LowerCtx) -> Result<LowerOutput, LowerError> {
        let name = arg_name(node).ok_or_else(|| LowerError::missing("package name"))?;
        let version = prop_str(node, "version");

        let select = match &version {
            None => NixExpr::Select(Box::new(NixExpr::Ref("pkgs".into())), vec![name.clone()]),
            Some(v) => {
                let pin = ctx.pin(&name, v).ok_or_else(|| {
                    LowerError::Other(format!(
                        "{name} {v} on {} is not resolved: run knixl install to pin it",
                        ctx.scope().host
                    ))
                })?;
                let url = format!(
                    "https://github.com/NixOS/nixpkgs/archive/{}.tar.gz",
                    pin.nixpkgs_rev
                );
                let mut src = std::collections::BTreeMap::new();
                src.insert(AttrKey::Ident("url".into()), NixExpr::Str(url));
                src.insert(AttrKey::Ident("sha256".into()), NixExpr::Str(pin.sha256.clone()));
                let fetch = NixExpr::Apply(
                    Box::new(NixExpr::Select(Box::new(NixExpr::Ref("builtins".into())), vec!["fetchTarball".into()])),
                    vec![NixExpr::AttrSet(src)],
                );
                let imported = NixExpr::Apply(
                    Box::new(NixExpr::Ref("import".into())),
                    vec![fetch, NixExpr::AttrSet(std::collections::BTreeMap::new())],
                );
                NixExpr::Select(Box::new(imported), vec![name.clone()])
            }
        };

        let assignment = Assignment {
            path: AttrPath(vec![
                AttrKey::Ident("environment".into()),
                AttrKey::Ident("systemPackages".into()),
            ]),
            value: NixExpr::List(vec![select]),
            priority: None,
            condition: None,
            doc: None,
        };
        Ok(LowerOutput::units(vec![Unit {
            bucket: Bucket::Default,
            assignment,
            module: String::new(),
        }]))
    }
```

Add a `prop_str` helper local to the file (returns `Option<String>` for the named prop):

```rust
fn prop_str(node: &KdlNode, key: &str) -> Option<String> {
    node.entries()
        .iter()
        .find(|e| e.name().map(|n| n.value()) == Some(key))
        .and_then(|e| e.value().as_string())
        .map(str::to_string)
}
```

Add the needed imports to the file's `use` lines: `NixExpr` (already), `AttrKey` (already), and confirm `LowerError::Other` exists (it does; used elsewhere). Import `Field`, `ValueTy` are already imported.

- [ ] **Step 5: Map lock pins into the pipeline `LowerCtx`**

In `crates/knixl-pipeline/src/lib.rs` `generate_one`, thread the host's pins. Add a `pins: &[knixl_lock::Pin]` parameter to `generate_one` and to `generate`/`generate_one`'s callers, mapping to `ResolvedPin` at the `LowerCtx::new` site:

```rust
        let resolved: Vec<knixl_modules::ResolvedPin> = pins
            .iter()
            .map(|p| knixl_modules::ResolvedPin {
                package: p.package.clone(),
                version: p.version.clone(),
                nixpkgs_rev: p.nixpkgs_rev.clone(),
                sha256: p.sha256.clone(),
            })
            .collect();
        let mut ctx = LowerCtx::new(
            Scope { host: host_name.clone() },
            registry,
            &mut diags,
            resolved,
        );
```

The public `generate(...)` signature gains `pins: &BTreeMap<String, Vec<knixl_lock::Pin>>`; it selects `pins.get(host_name).map(Vec::as_slice).unwrap_or(&[])` per host and passes it to `generate_one`. Note `host_name` is derived inside `generate_one` from the KDL, so pass the whole map into `generate_one` and let it look up its own host, OR compute the host name in `generate` before the call. Simplest: pass the whole `&BTreeMap` to `generate_one`, which looks up `pins.get(&host_name)`.

- [ ] **Step 6: Update `gather` to pass the lock's pins**

In `crates/knixl-pipeline/src/gather.rs`, the `generate(&hosts, &registry, formatter, &tool, oracle.as_ref())` call gains `&lock.pins`. Any other `generate(` call site (golden tests, `crates/knixl-pipeline/tests/*.rs`) passes an empty `&BTreeMap::new()` (bind it to a `let` so it lives long enough).

- [ ] **Step 7: Run tests + clippy**

Run: `cargo test -p knixl-modules -p knixl-pipeline 2>&1 | tail` (all pass incl. the 2 new package tests; existing goldens unchanged since unpinned emit is identical)
Run: `cargo clippy -p knixl-modules -p knixl-pipeline --all-targets 2>&1 | grep -cE 'warning:|error'` (expect 0)
Determinism: `cargo test -p knixl-pipeline` includes the twice-generate determinism test; confirm it still passes.

- [ ] **Step 8: Commit**

`but commit feat/version-pinning -c -m "feat(modules): emit pinned package imports from threaded lock pins" --changes <ids>`

---

### Task 4: `install pkg@version` parse, resolve, plain-path write (knixl-cli)

**Files:**
- Modify: `crates/knixl-pipeline/src/install.rs` (extend `add_package` to accept an optional version)
- Modify: `crates/knixl-cli/src/main.rs` (parse `@version`, resolve, plain-path write + lock pin)
- Modify: `crates/knixl-cli/tests/cli.rs` (integration test)

**Interfaces:**
- Consumes: `knixl_nix::pin::{PinResolver, Resolved, PinError}`, `knixl_lock::Pin`, existing `commit_install`, `add_package`.
- Produces: `add_package(src, pkg, version: Option<&str>) -> Result<Option<String>, String>` (version splices `package "name" version="v"`); a `write_pin(host, package, version, resolved)` lock update.

- [ ] **Step 1: Extend `add_package` with an optional version (TDD)**

In `crates/knixl-pipeline/src/install.rs`, change `add_package` to take `version: Option<&str>` and splice `package "name" version="v"` when present. Add a test:

```rust
#[test]
fn add_package_with_version_splices_a_version_prop() {
    let src = "host \"web\" {\n    system \"x86_64-linux\"\n}\n";
    let out = add_package(src, "htop", Some("3.2.1")).unwrap().expect("edit");
    assert!(out.contains("package \"htop\" version=\"3.2.1\""), "{out}");
    let doc: KdlDocument = out.parse().expect("valid kdl");
    assert!(doc.nodes().iter().any(|n| n.name().value() == "host"));
}
```

Implement by building the insertion line conditionally:
```rust
    let insertion = match version {
        Some(v) => format!("{indent}package \"{pkg}\" version=\"{v}\"\n"),
        None => format!("{indent}package \"{pkg}\"\n"),
    };
```
Update the existing `add_package(src, pkg)` call sites (in `main.rs` `commit_install`, `preview_host`, and the existing `add_package` tests) to pass `None`.

- [ ] **Step 2: Run the add_package test**

Run: `cargo test -p knixl-pipeline add_package 2>&1 | tail`
Expected: after Step 1, PASS (RED first if you write the test before changing the signature).

- [ ] **Step 3: Write the failing CLI integration test**

Read `crates/knixl-cli/tests/cli.rs` first and match its helpers (the existing `install_*` tests set `KNIXL_NIX` to a shim; add a resolver shim via `KNIXL_PIN_RESOLVER`). Add:

```rust
#[test]
fn install_pkg_at_version_refuses_when_resolver_cannot_find_it() {
    let proj = /* existing project scaffold helper */;
    let ok_eval = /* existing eval shim: resolves + parses */;
    let nf_resolver = write_resolver_shim(&proj, "nf", /*stdout*/ "", /*stderr*/ "not found", 1);
    let out = knixl(&proj)
        .args(["install", "htop@9.9.9", "--yes"])
        .env("KNIXL_NIX", &ok_eval)
        .env("KNIXL_PIN_RESOLVER", &nf_resolver)
        .output().unwrap();
    assert_eq!(out.status.code(), Some(5), "unresolvable version refuses (Validation): {out:?}");
}
```
Add a `write_resolver_shim` helper if none exists (a `#!/bin/sh` printing args-driven output and exiting a code), styled like the file's existing shim writers.

- [ ] **Step 4: Run to verify it fails**

Run: `cargo test -p knixl-cli --test cli install_pkg_at_version 2>&1 | tail`
Expected: FAIL (the `@version` path is not implemented).

- [ ] **Step 5: Implement `@version` parsing, resolution, and the plain-path write**

In `crates/knixl-cli/src/main.rs`:

- Parse `pkg@version`: split the `Install { pkg }` arg on the first `@` into `(name, Option<version>)`. Keep bare `pkg` behaviour identical.
- When a version is present, resolve it with `PinResolver::resolve().lookup(name, version)`. Map: `NotFound`/`Failed` -> print `knixl: <e>` + return `Code::Validation`; `Unavailable` -> print `knixl: cannot resolve <name>@<version>: <e>` + return `Code::Validation` (a pin cannot be created without resolution; unlike a skippable eval check, an unresolved pin is a hard stop even without `--strict`, because generation would be blocked).
- On success, in the plain path (`--yes` or non-TTY), after the existing resolve/confirm: write the KDL via `add_package(src, name, Some(version))`, and record the pin in the lock for the target host (`write_pin` below), then regenerate through the existing `commit_install` path (which reads the updated lock + KDL).
- Add `write_pin`: read the lock, insert/replace the `Pin { package, version, nixpkgs_rev, sha256 }` under the host (dedup by package: replace an existing pin for the same package), write the lock via the existing lock-writing helper.

The interactive TUI resolve/gate is Task 5; for this task, if interactive and a version is requested, it is acceptable to route `@version` through the plain confirm path temporarily behind a short-lived guard, OR gate Task 5 as the interactive path. Prefer: implement the resolution + write here so the plain path is fully working and tested; Task 5 adds the TUI surface.

- [ ] **Step 6: Run tests + clippy**

Run: `cargo test -p knixl-cli 2>&1 | tail` (all pass incl. the new CLI test)
Run: `cargo clippy -p knixl-cli --all-targets 2>&1 | grep -cE 'warning:|error'` (expect 0)

- [ ] **Step 7: Commit**

`but commit feat/version-pinning -c -m "feat(install): resolve and pin pkg@version (plain path)" --changes <ids>`

---

### Task 5: TUI Install screen pin row + gating (knixl-cli)

**Files:**
- Modify: `crates/knixl-cli/src/tui/mod.rs` (a `PinFn` + `Entry`/config plumbing, if needed)
- Modify: `crates/knixl-cli/src/tui/install.rs` (pin status, async resolve, gating, view row)
- Modify: `crates/knixl-cli/src/main.rs` (open the TUI for `@version`, inject the resolver fn, apply outcome writes the pin)

**Interfaces:** mirror the slice-B build pattern already in `install.rs` (`BuildState`, `begin_build`, `on_build_done`, spinner, gating, a build row). Add the analogous pin machinery.

- [ ] **Step 1: Write the failing reducer tests**

In `crates/knixl-cli/src/tui/install.rs` tests, add pin-state tests mirroring the build tests:

```rust
#[test]
fn pin_gating_blocks_apply_until_resolved() {
    let mut m = model(1);
    m.pin = PinState::Resolving;
    assert!(!m.apply_allowed(), "in-flight resolve blocks apply");
    m.pin = PinState::Failed;
    assert!(!m.apply_allowed(), "failed resolve blocks apply");
    m.pin = PinState::Resolved;
    assert!(m.apply_allowed(), "resolved allows apply");
}

#[test]
fn pin_off_does_not_affect_gating() {
    let mut m = model(1);
    m.pin = PinState::Off;
    assert!(m.apply_allowed());
}

#[test]
fn on_pin_done_ignores_stale() {
    let mut m = model(1);
    let seq = m.mark_resolving();
    m.mark_resolving();
    m.on_pin_done(seq, PinOutcome::Resolved);
    assert_eq!(m.pin, PinState::Resolving, "stale resolve discarded");
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p knixl-cli pin_ 2>&1 | tail`
Expected: FAIL to compile (`PinState`, `m.pin`, `mark_resolving`, `on_pin_done` absent).

- [ ] **Step 3: Implement the pin machinery (mirror the build machinery)**

Add to `install.rs`, exactly paralleling the slice-B build code that is already in this file:
- `enum PinState { Off, Resolving, Resolved, Failed }` and a `struct PinDone { seq, outcome }` and `enum` reuse `super::PinOutcome`.
- `InstallModel` fields `pin: PinState`, `pin_spinner: spinner::Model`, `pin_seq: u64`.
- `mark_resolving`, `begin_pin` (reads `config().pin` and the requested version; runs only when a version was requested; NOT re-run on host switch — like the build), `on_pin_done`.
- `apply_allowed` gains a `pin_ok` term: `Off | Resolved => true`, `Resolving | Failed => false`.
- `update` handles `PinDone` and routes the pin spinner tick by id (same id-routing form as the build spinner).
- `view` adds a `pin` row when `pin != PinState::Off` (spinner while resolving; `✓ pinned <shortrev>` / `✗ pin failed`), inserted next to the build row.
- `config()` gains `pin: Option<super::PinFn>` where `PinFn = Arc<dyn Fn(&str, &str) -> PinOutcome + Send + Sync>`, and `PinOutcome { Resolved { rev, sha256 } | NotFound | Unavailable | Failed }`. `Entry::Install` carries the requested `version: Option<String>`.

- [ ] **Step 4: Wire `main.rs` to open the TUI for `@version` and write the pin on Apply**

`install`'s interactive branch passes `version` into `Entry::Install` and injects a `PinFn` built from `PinResolver` (mirror `make_build`), closing over only `Send` data. The `Outcome::Install` handler, when a version was requested and resolved, calls `write_pin` (from Task 4) before `commit_install`. Under `--yes`/non-TTY, the Task 4 plain path already handles it.

- [ ] **Step 5: Run tests + clippy + a PTY smoke check**

Run: `cargo test -p knixl-cli 2>&1 | tail` (all pass)
Run: `cargo clippy -p knixl-cli --all-targets 2>&1 | grep -cE 'warning:|error'` (expect 0)
Smoke (optional, in `~/knixl-playground` with a resolver shim on PATH via `KNIXL_PIN_RESOLVER`): `knixl install htop@3.2.1` shows the pin row resolving then resolved.

- [ ] **Step 6: Commit**

`but commit feat/version-pinning -c -m "feat(tui): install pkg@version pin row, async resolve, and apply gating" --changes <ids>`

---

### Task 6: docs + full verification

**Files:**
- Modify: `docs/05-cli.md` (document `pkg@version`)

- [ ] **Step 1: Document it**

Extend the `knixl install` bullet: `knixl install pkg@version` pins a package version on the target host, resolving the version to a nixpkgs commit (via `KNIXL_PIN_RESOLVER`, default nixhub.io) at install time and recording it per host in the lock; the package is emitted from that pinned commit mixed into the host baseline; an unresolvable version refuses (exit 5); pairs with `--build` to catch cross-rev build breakage. Note the standing behaviour: interactive shows the pin in the TUI, `--yes`/non-TTY uses the plain confirm.

- [ ] **Step 2: Full workspace suite + clippy**

Run: `cargo test --workspace 2>&1 | grep -cE 'FAILED'` (expect 0)
Run: `cargo clippy --workspace --all-targets 2>&1 | grep -cE 'warning:|error'` (expect 0)

- [ ] **Step 3: Smoke the flag surface**

Run: `./target/debug/knixl install --help 2>&1 | grep -iE 'version|@'` (the help text mentions pkg@version)

- [ ] **Step 4: Commit**

`but commit feat/version-pinning -c -m "docs(cli): document knixl install pkg@version" --changes <ids>`

---

## Self-review notes

- Spec coverage: lock schema (Task 1), resolver (Task 2), version emit + threading (Task 3), install resolve + plain-path write (Task 4), TUI pin row + gating (Task 5), docs (Task 6). All covered.
- Dependency direction honoured: `ResolvedPin` lives in knixl-modules; the pipeline maps `knixl_lock::Pin` into it (Task 3 Step 5). knixl-modules never imports knixl-lock.
- Determinism: pins sorted (Task 1), `AttrSet` is a BTreeMap and the pinned import emits structurally so let-hoisting dedups (Task 3); the pipeline determinism test must stay green (Task 3 Step 7).
- Types consistent: `Pin` (knixl-lock) vs `ResolvedPin` (knixl-modules) vs `Resolved`/`PinError` (knixl-nix) vs `PinState`/`PinOutcome`/`PinFn` (tui) are distinct by design; the pipeline and CLI do the mapping between layers.
- `--build` and pinning compose (Task 5 view has both rows; gating ANDs both). `pkg@version` absent leaves all paths unchanged (version is `None`, `PinState::Off`).
