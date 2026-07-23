# Secrets model + tailscale module Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `(secret)"name"` template value form that emits a reference to a decrypted secret path (`config.<backend>.secrets."name".path`), make the backend a project-level setting (default sops-nix), and ship a declarative `tailscale` module that uses it.

**Architecture:** The `(secret)` form lives in the declarative template grammar (`knixl-modules`). It resolves to a `NixExpr::Raw` reference, threaded with a `SecretsBackend` that flows project config → `generate` → `LowerCtx` → the template interpreter. `tailscale` is a declarative module (no Rust). knixl never sees plaintext; the emitted reference is the whole record.

**Tech Stack:** Rust, the `kdl` crate, `knixl_ir::{NixExpr, RawNix}`, the existing declarative-module machinery.

## Global Constraints

Copied from the spec (`docs/superpowers/specs/2026-07-23-secrets-tailscale-design.md`) and repo house rules. Every task implicitly includes these:

- The repo IS rustfmt-normalised; CI runs `cargo fmt --all --check`. Run `cargo fmt` before every commit and keep it clean.
- Reference-only, no plaintext: knixl emits `config.<backend>.secrets."<name>".path` and never reads, stores, or hashes secret material. No secret declaration node, no known-name set, no dangling-name check (inline reference by design).
- The secret reference MUST be `NixExpr::Raw`, never `NixExpr::Select`: the emitter renders `Select` segments with a bare dot (`config.sops.secrets.tailscale-authkey.path`), which is invalid Nix for a hyphenated name. `Raw` passes through verbatim. The name is double-quoted and escaped (`\` and `"`).
- Backend prefixes: `SopsNix` → `config.sops.secrets."<n>".path`; `Agenix` → `config.age.secrets."<n>".path`. Default is `SopsNix`.
- Determinism is load-bearing: no `HashMap` on any emit path. The `(secret)` form is a pure function of (resolved name, backend).
- `services.tailscale.{enable,extraUpFlags,authKeyFile}` are stock in-tree options; no oracle-module dependency is added.
- Emit source text not values (ADR 0002); KDL is authoritative (ADR 0001). No lock format change.
- British spelling in prose/comments; no em-dashes or en-dashes; no banned AI-tell vocabulary (passionate, leverage, robust, seamless, delve, comprehensive, streamline, unlock, realm, landscape, testament, foster, etc.).
- Implementers: leave changes uncommitted, run no git/`but` command (including `git stash`). The controller commits.

---

### Task 1: The `(secret)` value form and backend threading

**Files:**
- Modify: `crates/knixl-modules/src/lib.rs` (add `SecretsBackend`; give `LowerCtx` a backend field, a builder, and a getter)
- Modify: `crates/knixl-modules/src/template.rs` (add `ValueTemplate::Secret`, parse it, thread the backend through `interpret`/`run`/`ValueTemplate::interpret`, dry-check it, tests)

**Interfaces:**
- Produces: `pub enum SecretsBackend { SopsNix, Agenix }` (`Clone, Copy, PartialEq, Eq, Debug, Default`; `SopsNix` is `#[default]`); `LowerCtx::with_secrets_backend(self, SecretsBackend) -> Self` and `LowerCtx::secrets_backend(&self) -> SecretsBackend`; `ValueTemplate::Secret(Vec<StrPart>)`.
- Consumes: `knixl_ir::RawNix` (already imported in template.rs).
- Task 2 calls `LowerCtx::with_secrets_backend` from `generate_one`; Task 3's tailscale module uses `(secret)` in its emit template.

- [ ] **Step 1: Add the `SecretsBackend` enum and wire it into `LowerCtx`**

In `crates/knixl-modules/src/lib.rs`, add the enum near `ResolvedPin`/`PinStrategy`:

```rust
/// Which secret manager a `(secret)` reference resolves against. sops-nix emits
/// `config.sops.secrets.<name>.path`; agenix emits `config.age.secrets.<name>.path`.
/// Set once per project (knixl.kdl `secrets backend="..."`); defaults to sops-nix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SecretsBackend {
    #[default]
    SopsNix,
    Agenix,
}
```

Add a field to `LowerCtx` (keep the existing fields):

```rust
pub struct LowerCtx<'a> {
    scope: Scope,
    registry: &'a Registry,
    diags: &'a mut Vec<Diagnostic>,
    pins: Vec<ResolvedPin>,
    secrets_backend: SecretsBackend,
}
```

In `LowerCtx::new`, initialise it to the default (do NOT add a constructor parameter, so existing call sites are unchanged; the default is the documented default backend):

```rust
    pub fn new(
        scope: Scope,
        registry: &'a Registry,
        diags: &'a mut Vec<Diagnostic>,
        pins: Vec<ResolvedPin>,
    ) -> Self {
        Self {
            scope,
            registry,
            diags,
            pins,
            secrets_backend: SecretsBackend::SopsNix,
        }
    }

    /// Override the secrets backend (the pipeline sets this from the project config).
    pub fn with_secrets_backend(mut self, backend: SecretsBackend) -> Self {
        self.secrets_backend = backend;
        self
    }

    /// The secrets backend a `(secret)` reference resolves against.
    pub fn secrets_backend(&self) -> SecretsBackend {
        self.secrets_backend
    }
```

- [ ] **Step 2: Write the failing tests in template.rs**

Add these to `template.rs`'s `#[cfg(test)] mod tests`. They build a declarative module whose emit uses `(secret)` and lower it under a chosen backend. Use the existing test helpers if present; otherwise these are self-contained:

```rust
    fn lower_with_backend(src: &str, node_src: &str, backend: crate::SecretsBackend) -> LowerOutput {
        let doc = src.parse::<kdl::KdlDocument>().expect("parse module");
        let module =
            DeclarativeModule::from_kdl(&doc, std::path::Path::new("t")).expect("from_kdl");
        let n = node_src
            .parse::<kdl::KdlDocument>()
            .unwrap()
            .nodes()
            .first()
            .unwrap()
            .clone();
        let reg = crate::Registry::new();
        let mut diags = Vec::new();
        let mut ctx = crate::LowerCtx::new(
            crate::Scope { host: "h".into() },
            &reg,
            &mut diags,
            vec![],
        )
        .with_secrets_backend(backend);
        module.lower(&n, &mut ctx).expect("lower")
    }

    const SECRET_MODULE: &str = "module name=\"t\" version=\"1.0.0\" {\n\
        \x20   claims-node \"t\"\n\
        \x20   schema {\n\
        \x20       child \"key\" repeated=#true {\n\
        \x20           prop \"secret\" type=\"string\" required=#true\n\
        \x20       }\n\
        \x20   }\n\
        \x20   emit {\n\
        \x20       for-each \"k\" in \"key\" {\n\
        \x20           set \"services.x.file\" (secret)\"{k.secret}\"\n\
        \x20       }\n\
        \x20   }\n\
        }";

    fn raw_src(out: &LowerOutput) -> String {
        match &out.units[0].assignment.value {
            NixExpr::Raw(r) => r.src.clone(),
            other => panic!("expected Raw, got {other:?}"),
        }
    }

    #[test]
    fn secret_sops_backend_emits_sops_path() {
        let out = lower_with_backend(
            SECRET_MODULE,
            "t {\n    key secret=\"tailscale-authkey\"\n}",
            crate::SecretsBackend::SopsNix,
        );
        assert_eq!(
            raw_src(&out),
            "config.sops.secrets.\"tailscale-authkey\".path"
        );
    }

    #[test]
    fn secret_agenix_backend_emits_age_path() {
        let out = lower_with_backend(
            SECRET_MODULE,
            "t {\n    key secret=\"tailscale-authkey\"\n}",
            crate::SecretsBackend::Agenix,
        );
        assert_eq!(
            raw_src(&out),
            "config.age.secrets.\"tailscale-authkey\".path"
        );
    }

    #[test]
    fn secret_name_is_escaped() {
        let out = lower_with_backend(
            SECRET_MODULE,
            "t {\n    key secret=\"a\\\"b\"\n}",
            crate::SecretsBackend::SopsNix,
        );
        // The embedded quote must be backslash-escaped so it cannot break out of the literal.
        assert_eq!(raw_src(&out), "config.sops.secrets.\"a\\\"b\".path");
    }

    #[test]
    fn secret_non_scalar_name_fails_dry_check() {
        // `{key}` (the whole repeated child, a list/scope) is not a scalar, so the module must
        // fail to load.
        let src = "module name=\"t\" version=\"1.0.0\" {\n\
            \x20   claims-node \"t\"\n\
            \x20   schema { child \"key\" repeated=#true { prop \"secret\" type=\"string\" required=#true } }\n\
            \x20   emit { set \"services.x.file\" (secret)\"{key}\" }\n\
            }";
        let doc = src.parse::<kdl::KdlDocument>().unwrap();
        assert!(DeclarativeModule::from_kdl(&doc, std::path::Path::new("t")).is_err());
    }
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p knixl-modules secret`
Expected: FAIL to COMPILE (no `ValueTemplate::Secret`, `(secret)` unparsed). That is the expected red for a new grammar form.

- [ ] **Step 4: Implement the grammar form and thread the backend**

In `template.rs`:

(a) Add the variant to `enum ValueTemplate` (after `CollectOpt`):

```rust
    Secret(Vec<StrPart>), // (secret)"name" -> config.<backend>.secrets."name".path
```

(b) `parse_value`: add an arm alongside `Some("collect-opt")`:

```rust
        Some("secret") => {
            let s = entry
                .value()
                .as_string()
                .ok_or_else(|| LowerError::Other("secret needs a name string".into()))?;
            Ok(ValueTemplate::Secret(parse_str_parts(s)))
        }
```

(c) Thread the backend. Change these signatures to take `backend: crate::SecretsBackend` (it is `Copy`):

- `EmitTemplate::interpret(&self, b: &Bindings) -> ...` becomes `interpret(&self, b: &Bindings, backend: crate::SecretsBackend) -> ...`, and its call to `self.run(&self.stmts, b, &mut loops, None, &mut units)` becomes `self.run(&self.stmts, b, &mut loops, None, &mut units, backend)`.
- `fn run(&self, stmts, b, loops, cond, out)` gains a trailing `backend: crate::SecretsBackend` param; pass `backend` through at every recursive `self.run(...)` call (the `WhenFlag`, `WhenConfig`, `ForEach`, `List` arms) and at the `value.interpret(b, loops)` call in `Stmt::Set`, which becomes `value.interpret(b, loops, backend)`.
- `ValueTemplate::interpret(&self, b, loops)` gains a trailing `backend: crate::SecretsBackend` param.

In `Stmt::List`'s body, the inner `self.run(body, b, loops, None, &mut elem_units)` also gains `backend`.

(d) `ValueTemplate::interpret`: add the `Secret` arm:

```rust
            ValueTemplate::Secret(parts) => {
                let name = interp_parts(parts, b, loops)?;
                let prefix = match backend {
                    crate::SecretsBackend::SopsNix => "sops.secrets",
                    crate::SecretsBackend::Agenix => "age.secrets",
                };
                let esc = name.replace('\\', "\\\\").replace('"', "\\\"");
                NixExpr::Raw(RawNix {
                    src: format!("config.{prefix}.\"{esc}\".path"),
                    span: None,
                })
            }
```

(e) `DeclarativeModule::lower`: pass the backend from ctx. Replace `self.template.interpret(&bindings)` with:

```rust
        self.template.interpret(&bindings, _ctx.secrets_backend())
```

and rename the `_ctx` parameter to `ctx` (it is now used).

(f) Dry type-pass: in `check_stmts`, the `Stmt::Set` arm's `match value`, add a `Secret` arm treated like `Str` (each interpolation must be a scalar). Update the existing `ValueTemplate::Str(parts) | ValueTemplate::IndentStr(parts)` arm to also cover `Secret`, or add a parallel arm:

```rust
                    ValueTemplate::Str(parts)
                    | ValueTemplate::IndentStr(parts)
                    | ValueTemplate::Secret(parts) => {
                        for part in parts {
                            if let StrPart::Interp(lk) = part {
                                expect_scalar(&lk.0, shapes, loops, errors);
                            }
                        }
                    }
```

(g) If any other `match` over `ValueTemplate` is now non-exhaustive (the compiler will point to it), add a `Secret` arm consistent with the `Str` handling.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p knixl-modules`
Expected: the four new `secret_*` tests pass and the whole crate suite stays green.

- [ ] **Step 6: fmt + clippy**

Run: `cargo fmt --all && cargo fmt --all --check && cargo clippy -p knixl-modules --all-targets`
Expected: clean.

- [ ] **Step 7: Report** (implementer writes the report file; controller commits)

---

### Task 2: Project backend config and pipeline plumbing

**Files:**
- Modify: `crates/knixl-pipeline/src/project.rs` (parse `secrets backend="..."`, add `ProjectConfig.secrets_backend`, test)
- Modify: `crates/knixl-pipeline/src/lib.rs` (`generate`/`generate_one` gain a `secrets_backend` param; `generate_one` applies it to the `LowerCtx`)
- Modify: `crates/knixl-pipeline/src/gather.rs` (pass `project.secrets_backend` into `generate`)
- Modify: `crates/knixl/src/main.rs` (the one `generate` call in `preview_host`)
- Modify: `crates/knixl-pipeline/tests/golden.rs` (the seven `generate` call sites)

**Interfaces:**
- Consumes: `knixl_modules::SecretsBackend` and `LowerCtx::with_secrets_backend` from Task 1.
- Produces: `ProjectConfig.secrets_backend: knixl_modules::SecretsBackend`; `generate(hosts, registry, formatter, tool, oracles, pins, secrets_backend)` (new trailing param). Task 3's golden relies on the default (sops-nix) path and passes `Agenix` explicitly for one test.

- [ ] **Step 1: Write the failing project-parse tests**

In `crates/knixl-pipeline/src/project.rs`'s `#[cfg(test)] mod tests`, add:

```rust
    #[test]
    fn secrets_backend_defaults_to_sops() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("knixl.kdl"), "nixpkgs release=\"25.05\"\n").unwrap();
        let cfg = parse_project(dir.path()).unwrap();
        assert_eq!(cfg.secrets_backend, knixl_modules::SecretsBackend::SopsNix);
    }

    #[test]
    fn secrets_backend_agenix_parses() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("knixl.kdl"), "secrets backend=\"agenix\"\n").unwrap();
        let cfg = parse_project(dir.path()).unwrap();
        assert_eq!(cfg.secrets_backend, knixl_modules::SecretsBackend::Agenix);
    }

    #[test]
    fn secrets_backend_unknown_errors() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("knixl.kdl"), "secrets backend=\"vault\"\n").unwrap();
        assert!(parse_project(dir.path()).is_err());
    }
```

This mirrors the existing `parse_project` tests in this module (they use `tempfile::tempdir()` and `std::fs::write(dir.path().join("knixl.kdl"), ...)`).

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p knixl-pipeline secrets_backend`
Expected: FAIL to compile (`secrets_backend` field and enum variant not referenced yet) or assertion failure.

- [ ] **Step 3: Add the field and parse it**

In `project.rs`:

Add to `ProjectConfig` (keep existing fields):

```rust
    pub secrets_backend: knixl_modules::SecretsBackend,
```

`ProjectConfig` derives `Default`; `SecretsBackend`'s default is `SopsNix`, so `#[derive(Default)]` still works.

Add a `ProjectError` variant:

```rust
    #[error("knixl.kdl: unknown secrets backend `{0}` (expected `sops-nix` or `agenix`)")]
    UnknownSecretsBackend(String),
```

In `parse_project`, after the `oracle_modules` block and before building the result, parse the backend:

```rust
    let secrets_backend = match doc
        .nodes()
        .iter()
        .find(|n| n.name().value() == "secrets")
        .and_then(|n| n.get("backend"))
        .and_then(|v| v.as_string())
    {
        None => knixl_modules::SecretsBackend::SopsNix,
        Some("sops-nix") => knixl_modules::SecretsBackend::SopsNix,
        Some("agenix") => knixl_modules::SecretsBackend::Agenix,
        Some(other) => return Err(ProjectError::UnknownSecretsBackend(other.to_string())),
    };
```

Add `secrets_backend` to the returned `ProjectConfig { .. }`.

Confirm `knixl-pipeline`'s `Cargo.toml` already depends on `knixl-modules` (it does; `generate` uses `Registry`/`LowerCtx`). If `knixl_modules` is not already imported in `project.rs`, refer to it by full path `knixl_modules::SecretsBackend` as above (no `use` needed).

- [ ] **Step 4: Thread the backend through `generate`**

In `crates/knixl-pipeline/src/lib.rs`:

Add a trailing parameter to both `generate` and `generate_one`:

```rust
    secrets_backend: knixl_modules::SecretsBackend,
```

`generate` forwards it to `generate_one`. In `generate_one`, find the `LowerCtx::new(...)` construction and append the builder call so the backend reaches the interpreter:

```rust
        let mut ctx = LowerCtx::new(
            Scope { host: host_name.clone() },
            registry,
            &mut diags,
            resolved_pins.clone(),
        )
        .with_secrets_backend(secrets_backend);
```

(Keep everything else about the `ctx` construction as-is.)

- [ ] **Step 5: Update every `generate` call site**

The compiler will flag each. Update:

- `crates/knixl-pipeline/src/gather.rs` (the `generate(&hosts, &registry, formatter, &tool, &oracles, &lock.pins)` call): pass `project.secrets_backend` as the final argument. `gather` already binds `project` from `parse_project`; if the binding is named differently in scope, use that name.
- `crates/knixl/src/main.rs` `preview_host`: pass `knixl_modules::SecretsBackend::default()` (preview is best-effort and secret-agnostic; wiring the project backend into preview is out of scope).
- `crates/knixl-pipeline/tests/golden.rs` (seven call sites): pass `knixl_modules::SecretsBackend::default()` as the final argument at each. (Import is available; use the full path.)

- [ ] **Step 6: Run the tests**

Run: `cargo test -p knixl-pipeline secrets_backend` then `cargo test -p knixl-pipeline` and `cargo test -p knixl`.
Expected: the three project tests pass; the pipeline and CLI suites stay green (the new param is threaded, existing goldens unchanged because the default is sops-nix and no current module uses `(secret)`).

- [ ] **Step 7: fmt + clippy**

Run: `cargo fmt --all && cargo fmt --all --check && cargo clippy --workspace --all-targets`
Expected: clean.

- [ ] **Step 8: Report**

---

### Task 3: The tailscale module and the gateway golden

**Files:**
- Create: `modules/tailscale/knixl-module.kdl`
- Create: `examples/hosts/gateway.kdl`
- Create: `examples/expected/gateway.nix` (blessed with nixfmt, not hand-written)
- Modify: `crates/knixl-pipeline/tests/golden.rs` (structural, attribution, byte-exact, and an agenix-backend test)

**Interfaces:**
- Consumes: the `(secret)` form (Task 1), the `generate` `secrets_backend` param (Task 2), the existing golden harness helpers (`generate_host`, `assert_host_matches`, `formatter_available`, `build_registry`, `formatter`, `identity_formatter`, `examples_dir`).

- [ ] **Step 1: Write the tailscale module**

Create `modules/tailscale/knixl-module.kdl`:

```kdl
module name="tailscale" version="1.0.0" {
    summary "Tailscale with an auth key from a named secret and optional up-flags."
    claims-node "tailscale"

    schema {
        child "up-flag" type="string" repeated=#true \
            doc="A flag appended to services.tailscale.extraUpFlags, e.g. \"--ssh\"."
        child "auth-key" repeated=#true \
            doc="Wire services.tailscale.authKeyFile to a named secret. At most one." {
            prop "secret" type="string" required=#true
        }
    }

    emit {
        set "services.tailscale.enable" #true
        set "services.tailscale.extraUpFlags" (collect-opt)"up-flag"
        for-each "k" in "auth-key" {
            set "services.tailscale.authKeyFile" (secret)"{k.secret}"
        }
    }
}
```

The golden harness's `build_registry` auto-discovers `modules/*/knixl-module.kdl`, so no registration code is needed.

- [ ] **Step 2: Write the gateway host**

Create `examples/hosts/gateway.kdl`:

```kdl
host "gateway" {
    system "x86_64-linux"

    tailscale {
        up-flag "--ssh"
        auth-key secret="tailscale-authkey"
    }
}
```

- [ ] **Step 3: Add the tests**

Add to `crates/knixl-pipeline/tests/golden.rs` (mirror the `nas_*` triplet). The agenix test drives the full backend thread through `generate`:

```rust
#[test]
fn gateway_pipeline_produces_expected_structure() {
    let files = generate_host("gateway.kdl");
    assert_eq!(files.len(), 1, "gateway has no side-files");
    let text = &files[0].text;
    for needle in [
        "services.tailscale.enable = true",
        "services.tailscale.extraUpFlags",
        "\"--ssh\"",
        "services.tailscale.authKeyFile = config.sops.secrets.\"tailscale-authkey\".path",
    ] {
        assert!(text.contains(needle), "gateway.nix missing `{needle}`\n---\n{text}");
    }
}

#[test]
fn gateway_file_attributes_tailscale() {
    let files = generate_host("gateway.kdl");
    let gw = &files[0];
    for m in ["host", "tailscale"] {
        assert!(
            gw.modules.contains(&m.to_string()),
            "gateway.nix should list {m}, got {:?}",
            gw.modules
        );
    }
}

#[test]
fn gateway_agenix_backend_emits_age_path() {
    // The project-level backend flows generate -> LowerCtx -> the (secret) form.
    let examples = examples_dir();
    let path = PathBuf::from("hosts").join("gateway.kdl");
    let src = fs::read_to_string(examples.join(&path)).expect("read host kdl");
    let tool = "0.3.1".parse().unwrap();
    let no_pins = std::collections::BTreeMap::new();
    let no_oracles = std::collections::BTreeMap::new();
    let files = generate(
        &[HostSource { path, src }],
        &build_registry(),
        &identity_formatter(),
        &tool,
        &no_oracles,
        &no_pins,
        knixl_modules::SecretsBackend::Agenix,
    )
    .expect("generate");
    assert!(
        files[0].text.contains(
            "services.tailscale.authKeyFile = config.age.secrets.\"tailscale-authkey\".path"
        ),
        "agenix backend should emit an age path\n---\n{}",
        files[0].text
    );
}

#[test]
fn gateway_matches_golden() {
    if !formatter_available() {
        eprintln!("skipping gateway_matches_golden: no formatter (set KNIXL_FORMATTER)");
        return;
    }
    assert_host_matches("gateway.kdl");
}
```

If `knixl_modules` is not already imported in `golden.rs`, add `use knixl_modules;` or refer to it by full path as written above.

- [ ] **Step 4: Run the structural + attribution + agenix tests (identity formatter)**

Run: `cargo test -p knixl-pipeline gateway_pipeline_produces_expected_structure gateway_file_attributes_tailscale gateway_agenix_backend_emits_age_path`
Expected: all pass. Fix the module/emit if a needle is missing.

- [ ] **Step 5: Bless the byte-exact golden**

Same procedure the disko golden used:

1. Confirm the local formatter reproduces an existing golden:
   `KNIXL_FORMATTER=$(command -v nixfmt) cargo test -p knixl-pipeline nas_matches_golden -- --nocapture`
   Expected PASS. If it FAILS, STOP and report BLOCKED (local formatter differs from the pinned one; do not hand-write the expected file).
2. Add a temporary bless test:

```rust
#[test]
#[ignore]
fn bless_gateway() {
    let examples = examples_dir();
    let path = PathBuf::from("hosts").join("gateway.kdl");
    let src = fs::read_to_string(examples.join(&path)).unwrap();
    let tool = "0.3.1".parse().unwrap();
    let no_pins = std::collections::BTreeMap::new();
    let no_oracles = std::collections::BTreeMap::new();
    let files = generate(
        &[HostSource { path, src }],
        &build_registry(),
        &formatter(),
        &tool,
        &no_oracles,
        &no_pins,
        knixl_modules::SecretsBackend::default(),
    )
    .expect("generate");
    fs::write(examples.join("expected/gateway.nix"), &files[0].text).unwrap();
}
```

   Run: `KNIXL_FORMATTER=$(command -v nixfmt) cargo test -p knixl-pipeline bless_gateway -- --ignored --nocapture`
3. Open `examples/expected/gateway.nix` and sanity-check: valid Nix; `services.tailscale.enable = true`; `extraUpFlags` containing `"--ssh"`; `services.tailscale.authKeyFile = config.sops.secrets."tailscale-authkey".path`.
4. REMOVE the `bless_gateway` test so it is not in the committed diff.

- [ ] **Step 6: Verify the golden and full suite**

Run: `KNIXL_FORMATTER=$(command -v nixfmt) cargo test -p knixl-pipeline gateway`
Then: `cargo test --workspace && cargo fmt --all --check && cargo clippy --workspace --all-targets`
Expected: all green, `bless_gateway` gone.

- [ ] **Step 7: Report** (confirm the bless test was removed)

---

### Task 4: Docs

**Files:**
- Modify: `docs/04-template-grammar.md` (the `(secret)` value form under Values; tailscale in the module list)
- Modify: `docs/06-oracle.md` (a note that a secret reference is a value, not an option path)

**Interfaces:** none (prose only).

- [ ] **Step 1: Read both docs**

Read `docs/04-template-grammar.md` (find the Values section listing `collect`/`collect-opt`/`indent-str`, and the module list where disko/others are documented) and `docs/06-oracle.md` (find the out-of-tree modules section).

- [ ] **Step 2: Document the `(secret)` form and tailscale in docs/04**

In the Values section, in the same style as the neighbouring value forms, add:

- `(secret)"name"` : emits a reference to a decrypted secret path, `config.<backend>.secrets."name".path`. The name may interpolate bindings (`(secret)"{k.secret}"`). The backend is the project's `secrets backend=` setting (default sops-nix); it is emitted as a `config.*` reference, so knixl never sees the secret material. Reference-only: there is no secret declaration and no name validation.

In the module list, add `tailscale`: a declarative module claiming the `tailscale` node, setting `services.tailscale.enable`, collecting `up-flag` children into `extraUpFlags`, and wiring `authKeyFile` to an `auth-key secret="name"` via `(secret)`.

- [ ] **Step 3: Note the secret-reference/oracle boundary in docs/06**

Add a short note: a `(secret)` reference emits a `config.<backend>.secrets.*` value, not an option path, so it is not oracle-validated (the oracle checks the option paths a module assigns, e.g. `services.tailscale.authKeyFile`, not the values). The backend (`sops-nix` or `agenix`) is set by the project `secrets backend=` node in `knixl.kdl`.

- [ ] **Step 4: House-style self-check**

Grep your additions for em/en-dashes and banned words; British spelling. Fix any hit.

- [ ] **Step 5: Report**

---

## Notes for the controller

- Base commit before Task 1: the tip of `feat/secrets-tailscale` (the spec commit). Record it; each task's start commit is the BASE for the next task's review package.
- Task 1 (grammar + threading) and Task 2 (project config + pipeline plumbing) are separable review gates: Task 1 proves `(secret)` under both backends via direct lowering; Task 2 proves the project-config parse and that the param compiles through every call site; Task 3 proves the full end-to-end thread (knixl.kdl backend → gateway.nix) including the agenix path.
- The final whole-branch review should confirm: the secret reference is `Raw` (never `Select`) and correctly escaped; no `HashMap` on the emit path; the backend default is sops-nix; `services.tailscale.*` needs no oracle module; the golden was blessed (not hand-written) and `nas_matches_golden` passed under the same formatter; `cargo fmt --all --check` and `cargo clippy --workspace --all-targets` clean; workspace suite green.
- This branch is stacked on `feat/disko-module`. Keep commits on `feat/secrets-tailscale`; do not touch the disko branch.
