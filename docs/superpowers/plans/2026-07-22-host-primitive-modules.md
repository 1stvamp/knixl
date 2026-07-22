# Host primitive modules (zfs, user, openssh) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship three general-purpose declarative modules (zfs, user, openssh) plus the one small grammar feature (`(collect-opt)`) they need, so a host can express these primitives without `raw-nix`.

**Architecture:** Three `modules/<name>/knixl-module.kdl` manifests using the existing template grammar, enabled by an additive `(collect-opt)` value form that emits a list assignment only when the repeated child is non-empty. One golden example host (`nas.kdl`) exercises all three end to end.

**Tech Stack:** Rust workspace (knixl-modules, knixl-pipeline), KDL manifests, nixfmt for the byte-exact golden.

Spec: `docs/superpowers/specs/2026-07-21-host-primitive-modules-design.md`.

## Global Constraints

- British spelling in all prose/comments. No em-dashes or en-dashes: use colons, parentheses, commas, full stops. No banned AI vocab (see CLAUDE.md).
- Determinism to the byte: emit paths use `BTreeMap`/`Vec`/index-preserving order only, never `HashMap`. Output must be a pure function of KDL source order.
- `(collect)` behaviour is unchanged; `(collect-opt)` is additive and must not move any existing golden (`web`, `shared`, `db`, `pinned`, `pinned-override`).
- No lock or oracle change: these are stock NixOS options.
- The repo is rustfmt-normalised and CI runs `cargo fmt --all --check`. Run `cargo fmt --all` and keep the tree clean.
- Implementers do NOT run any git/`but` commands and do NOT commit. Leave changes uncommitted; the controller commits.

---

### Task 1: `(collect-opt)` grammar feature

**Files:**
- Modify: `crates/knixl-modules/src/template.rs`
- Modify: `docs/04-template-grammar.md`
- Test: `crates/knixl-modules/src/template.rs` (the existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: nothing new.
- Produces: `ValueTemplate::CollectOpt(String)`, a value form written in a manifest as `(collect-opt)"<child>"`. Semantics: a flat `List` of the repeated child's first args (identical to `Collect` when non-empty); when the child is empty the enclosing `set` emits **no** assignment at all.

- [ ] **Step 1: Write the failing unit tests**

Add to `crates/knixl-modules/src/template.rs` inside `mod tests` (reuse the existing `lower`, `find`, `node`, `DeclarativeModule::from_kdl` helpers, following `list_with_an_absent_child_is_empty`):

```rust
#[test]
fn collect_opt_emits_the_list_when_present() {
    let manifest = "module name=\"co\" version=\"0.1.0\" {\n    claims-node \"co\"\n    schema {\n        child \"item\" type=\"string\" repeated=#true\n    }\n    emit {\n        set \"a.b\" (collect-opt)\"item\"\n    }\n}";
    let doc = manifest.parse::<kdl::KdlDocument>().unwrap();
    let m = DeclarativeModule::from_kdl(&doc, std::path::Path::new("co")).expect("loads");
    let out = lower(&m, &node("co {\n    item \"x\"\n    item \"y\"\n}"));
    match find(&out, "a.b") {
        Some(NixExpr::List(items)) => assert_eq!(items.len(), 2),
        other => panic!("a.b = {other:?}"),
    }
}

#[test]
fn collect_opt_emits_nothing_when_empty() {
    let manifest = "module name=\"co\" version=\"0.1.0\" {\n    claims-node \"co\"\n    schema {\n        child \"item\" type=\"string\" repeated=#true\n    }\n    emit {\n        set \"a.b\" (collect-opt)\"item\"\n    }\n}";
    let doc = manifest.parse::<kdl::KdlDocument>().unwrap();
    let m = DeclarativeModule::from_kdl(&doc, std::path::Path::new("co")).expect("loads");
    let out = lower(&m, &node("co"));
    assert!(find(&out, "a.b").is_none(), "empty collect-opt emits no assignment");
}

#[test]
fn collect_still_emits_empty_list_when_empty() {
    // Guard the non-breaking promise: plain (collect) is unchanged.
    let manifest = "module name=\"c\" version=\"0.1.0\" {\n    claims-node \"c\"\n    schema {\n        child \"item\" type=\"string\" repeated=#true\n    }\n    emit {\n        set \"a.b\" (collect)\"item\"\n    }\n}";
    let doc = manifest.parse::<kdl::KdlDocument>().unwrap();
    let m = DeclarativeModule::from_kdl(&doc, std::path::Path::new("c")).expect("loads");
    let out = lower(&m, &node("c"));
    match find(&out, "a.b") {
        Some(NixExpr::List(items)) => assert!(items.is_empty()),
        other => panic!("a.b = {other:?}"),
    }
}

#[test]
fn dry_check_rejects_collect_opt_over_a_non_repeated_child() {
    let manifest = "module name=\"bad\" version=\"0.1.0\" {\n    claims-node \"bad\"\n    schema {\n        child \"item\" type=\"string\"\n    }\n    emit {\n        set \"a.b\" (collect-opt)\"item\"\n    }\n}";
    let doc = manifest.parse::<kdl::KdlDocument>().unwrap();
    let err = DeclarativeModule::from_kdl(&doc, std::path::Path::new("bad")).err().unwrap();
    assert!(format!("{err}").contains("not a repeated child"), "got: {err}");
}
```

- [ ] **Step 2: Run the tests, verify they fail**

Run: `cargo test -p knixl-modules collect_opt`
Expected: FAIL. Loading `(collect-opt)` errors with `unknown value annotation` (parse_value has no such arm), so `from_kdl` returns `Err` and the tests panic at `.expect("loads")`.

- [ ] **Step 3: Add the `CollectOpt` variant**

In `crates/knixl-modules/src/template.rs`, extend `enum ValueTemplate` (after `Collect`):

```rust
pub enum ValueTemplate {
    Bool(bool),
    Int(i128),
    Str(Vec<StrPart>),       // "{upstream}"
    IndentStr(Vec<StrPart>), // (indent-str #""" ... """#)
    Collect(String),         // (collect "alias") -> List of that child's first arg
    CollectOpt(String),      // (collect-opt "x") -> like Collect, but the set is dropped when empty
}
```

- [ ] **Step 4: Parse the `(collect-opt)` annotation**

In `parse_value`, add an arm next to the `Some("collect")` arm:

```rust
Some("collect-opt") => {
    let child = entry
        .value()
        .as_string()
        .ok_or_else(|| LowerError::Other("collect-opt needs a child name".into()))?;
    Ok(ValueTemplate::CollectOpt(child.to_string()))
}
```

- [ ] **Step 5: Interpret `CollectOpt` like `Collect`**

In `ValueTemplate::interpret`, fold the two variants into one arm (they resolve identically to a `NixExpr::List`):

```rust
ValueTemplate::Collect(child) | ValueTemplate::CollectOpt(child) => {
    let mut items = Vec::new();
    for item in resolve_list(child, b)? {
        match item {
            Binding::Scalar(s) => items.push(scalar_to_expr(s)),
            _ => {
                return Err(LowerError::Other(format!(
                    "collect `{child}` expects scalar items"
                )))
            }
        }
    }
    NixExpr::List(items)
}
```

- [ ] **Step 6: Drop the assignment when a `CollectOpt` resolves empty**

In `EmitTemplate::run`, replace the `Stmt::Set { path, value }` arm body so the value is interpreted once and an empty `CollectOpt` is skipped:

```rust
Stmt::Set { path, value } => {
    let v = value.interpret(b, loops)?;
    // An optional collect over an absent/empty child emits nothing, so it does not
    // clobber a NixOS default (e.g. services.openssh.ports defaults to [ 22 ]).
    if matches!(value, ValueTemplate::CollectOpt(_))
        && matches!(&v, NixExpr::List(items) if items.is_empty())
    {
        continue;
    }
    let a = Assignment {
        path: path.interpret(b, loops)?,
        value: v,
        priority: None,
        condition: cond.map(|c| {
            NixExpr::Raw(RawNix {
                src: c.to_string(),
                span: None,
            })
        }),
        doc: None,
    };
    out.push(Unit {
        bucket: Bucket::Default,
        assignment: a,
        module: String::new(),
    });
}
```

- [ ] **Step 7: Handle `CollectOpt` in the dry-check pass**

In the dry-check `match value` over `ValueTemplate` (the arm that today reads `ValueTemplate::Collect(child) =>`), fold in `CollectOpt`:

```rust
ValueTemplate::Collect(child) | ValueTemplate::CollectOpt(child) => {
    match lookup_shape(std::slice::from_ref(child), shapes, loops) {
        Ok(Shape::List(_)) => {}
        Ok(_) => errors.push(format!("collect `{child}` is not a repeated child")),
        Err(e) => errors.push(e),
    }
}
```

- [ ] **Step 8: Build and satisfy exhaustiveness**

Run: `cargo build -p knixl-modules`
Expected: compiles. If the compiler reports any other non-exhaustive `match` on `ValueTemplate`, add a `CollectOpt` arm mirroring `Collect` there. (Search: `rg "ValueTemplate::" crates/knixl-modules/src/template.rs`.)

- [ ] **Step 9: Run the tests, verify they pass**

Run: `cargo test -p knixl-modules collect_opt && cargo test -p knixl-modules collect_still_emits && cargo test -p knixl-modules dry_check_rejects_collect_opt`
Expected: PASS.

- [ ] **Step 10: Document `(collect-opt)` in the grammar doc**

In `docs/04-template-grammar.md`, under `## Values`, add a bullet after the `(collect)` bullet:

```markdown
- `(collect-opt)"child"` : like `(collect)`, but the whole `set` is omitted when the
  child is empty, rather than emitting `[ ]`. Use it for an optional list-valued option
  whose absence should leave the NixOS default in place (e.g. `services.openssh.ports`,
  which defaults to `[ 22 ]`). `(collect)` still emits `[ ]` when empty.
```

- [ ] **Step 11: Format, run the crate suite**

Run: `cargo fmt --all && cargo test -p knixl-modules`
Expected: PASS, tree clean.

---

### Task 2: The three module manifests and the golden host

**Files:**
- Create: `modules/zfs/knixl-module.kdl`
- Create: `modules/user/knixl-module.kdl`
- Create: `modules/openssh/knixl-module.kdl`
- Create: `examples/hosts/nas.kdl`
- Test: `crates/knixl-pipeline/tests/golden.rs`

**Interfaces:**
- Consumes: `(collect-opt)` from Task 1; the existing grammar (`set`, `when-flag`, `for-each`, `(collect)`).
- Produces: three registered modules (`zfs`, `user`, `openssh`) auto-discovered by `build_registry`, and `examples/hosts/nas.kdl` that uses all three.

- [ ] **Step 1: Write `modules/zfs/knixl-module.kdl`**

```kdl
// A declarative knixl module: enable ZFS with its mandatory hostId, plus optional
// scrubbing, extra pools and an ARC size cap. All stock NixOS options.

module name="zfs" version="1.0.0" {
    summary "Enable ZFS with the mandatory hostId, optional ARC cap and scrubbing."
    claims-node "zfs"

    schema {
        arg "host-id" type="string" required=#true \
            doc="8 hex-digit machine ID, mandatory for ZFS. Generate with: head -c4 /dev/urandom | od -A none -t x4"
        child "auto-scrub" type="bool" \
            doc="Enable periodic pool scrubbing (services.zfs.autoScrub.enable)."
        child "extra-pool" type="string" repeated=#true \
            doc="Pool name to import at boot (boot.zfs.extraPools)."
        child "arc-max-bytes" repeated=#true \
            doc="Cap the ARC at N bytes via boot.extraModprobeConfig. At most one." {
            arg "bytes" type="int" required=#true
        }
    }

    emit {
        set "networking.hostId" "{host-id}"
        set "boot.supportedFilesystems.zfs" #true
        set "boot.zfs.extraPools" (collect-opt)"extra-pool"
        when-flag "auto-scrub" {
            set "services.zfs.autoScrub.enable" #true
        }
        for-each "cap" in "arc-max-bytes" {
            set "boot.extraModprobeConfig" "options zfs zfs_arc_max={cap.bytes}"
        }
    }
}
```

- [ ] **Step 2: Write `modules/user/knixl-module.kdl`**

```kdl
// A declarative knixl module: a normal login user with an optional description,
// supplementary groups and authorised SSH keys. All stock NixOS options.

module name="user" version="1.0.0" {
    summary "A normal login user with groups and SSH authorised keys."
    claims-node "user"

    schema {
        arg "name" type="string" required=#true doc="Login name."
        child "description" repeated=#true \
            doc="Full name / GECOS field. At most one." {
            arg "text" type="string" required=#true
        }
        child "group" type="string" repeated=#true \
            doc="Supplementary group, e.g. wheel."
        child "ssh-key" type="string" repeated=#true \
            doc="Authorised SSH public key."
    }

    emit {
        set "users.users.{name}.isNormalUser" #true
        set "users.users.{name}.extraGroups" (collect-opt)"group"
        set "users.users.{name}.openssh.authorizedKeys.keys" (collect-opt)"ssh-key"
        for-each "d" in "description" {
            set "users.users.{name}.description" "{d.text}"
        }
    }
}
```

- [ ] **Step 3: Write `modules/openssh/knixl-module.kdl`**

```kdl
// A declarative knixl module: hardened OpenSSH (password auth off, key auth on)
// with optional listen ports and login knobs. All stock NixOS options.

module name="openssh" version="1.0.0" {
    summary "Hardened OpenSSH (password auth off) with port and login knobs."
    claims-node "openssh"

    schema {
        child "port" type="int" repeated=#true \
            doc="Listen port(s) (services.openssh.ports). NixOS default [ 22 ] if omitted."
        child "permit-root" repeated=#true \
            doc="PermitRootLogin value, e.g. \"no\" or \"prohibit-password\". At most one." {
            arg "value" type="string" required=#true
        }
        child "x11-forwarding" type="bool" \
            doc="Enable X11 forwarding (off by default)."
    }

    emit {
        set "services.openssh.enable" #true
        set "services.openssh.settings.PasswordAuthentication" #false
        set "services.openssh.settings.KbdInteractiveAuthentication" #false
        set "services.openssh.ports" (collect-opt)"port"
        when-flag "x11-forwarding" {
            set "services.openssh.settings.X11Forwarding" #true
        }
        for-each "r" in "permit-root" {
            set "services.openssh.settings.PermitRootLogin" "{r.value}"
        }
    }
}
```

- [ ] **Step 4: Write `examples/hosts/nas.kdl`**

Note the invocation of a repeated child that carries a single `arg`: it takes a
positional argument (`arc-max-bytes 8589934592`, `description "Wes Mason"`,
`permit-root "prohibit-password"`), exactly like web-service's `location "/api"`.

```kdl
host "nas" {
    system "x86_64-linux"

    zfs "8425e349" {
        auto-scrub #true
        extra-pool "tank"
        arc-max-bytes 8589934592
    }

    user "wes" {
        description "Wes Mason"
        group "wheel"
        ssh-key "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAExampleKeyForGoldenTest wes@nas"
    }

    openssh {
        port 22
        port 2222
        permit-root "prohibit-password"
    }
}
```

- [ ] **Step 5: Write the failing structural + attribution tests**

Add to `crates/knixl-pipeline/tests/golden.rs` (mirror `web_pipeline_produces_expected_structure` and `web_file_attributes_every_contributing_module`; both use the identity formatter via `generate_host`, so they run without nixfmt):

```rust
#[test]
fn nas_pipeline_produces_expected_structure() {
    let files = generate_host("nas.kdl");
    assert_eq!(files.len(), 1, "nas has no side-files");
    let text = &files[0].text;
    for needle in [
        "networking.hostId = \"8425e349\"",
        "boot.supportedFilesystems.zfs = true",
        "boot.zfs.extraPools",
        "services.zfs.autoScrub.enable = true",
        "options zfs zfs_arc_max=8589934592",
        "users.users.\"wes\".isNormalUser = true",
        "users.users.\"wes\".description = \"Wes Mason\"",
        "users.users.\"wes\".openssh.authorizedKeys.keys",
        "services.openssh.settings.PasswordAuthentication = false",
        "services.openssh.settings.KbdInteractiveAuthentication = false",
        "services.openssh.ports",
        "services.openssh.settings.PermitRootLogin = \"prohibit-password\"",
    ] {
        assert!(text.contains(needle), "nas.nix missing `{needle}`\n---\n{text}");
    }
    // openssh has no port omitted here, but the empty-collect-opt promise is unit-tested
    // in knixl-modules; here we assert the ports line IS present because ports were given.
}

#[test]
fn nas_file_attributes_every_contributing_module() {
    let files = generate_host("nas.kdl");
    let nas = &files[0];
    for m in ["host", "zfs", "user", "openssh"] {
        assert!(
            nas.modules.contains(&m.to_string()),
            "nas.nix should list {m}, got {:?}",
            nas.modules
        );
    }
}
```

- [ ] **Step 6: Run the tests, verify they pass**

Run: `cargo test -p knixl-pipeline --test golden nas_`
Expected: PASS. `build_registry` walks `modules/*`, so the three new manifests load and register; `generate_host("nas.kdl")` produces one file whose text contains every needle and whose `modules` lists all four.

If loading a manifest fails, the error surfaces from `build_registry`'s `.expect("load declarative module")`. Fix the manifest (not the test) and re-run.

- [ ] **Step 7: Format, run the full pipeline suite**

Run: `cargo fmt --all && cargo test -p knixl-pipeline`
Expected: PASS. Confirm the pre-existing goldens (`web`, `shared`, `db`, `pinned`) are unaffected by the added modules and host.

---

### Task 3: Byte-exact golden for nas.nix

**Files:**
- Create: `examples/expected/nas.nix` (generated, committed)
- Test: `crates/knixl-pipeline/tests/golden.rs`

**Interfaces:**
- Consumes: `examples/hosts/nas.kdl` and the three modules from Task 2; the real `nixfmt` (via `KNIXL_FORMATTER=nixfmt`).
- Produces: the committed golden `examples/expected/nas.nix` and a `nas_matches_golden` test.

- [ ] **Step 1: Add a one-off writer test to emit the golden**

Temporarily add this to `crates/knixl-pipeline/tests/golden.rs` (it uses the real `formatter()`, mirroring `assert_host_matches` but writing instead of asserting):

```rust
#[test]
#[ignore]
fn write_nas_golden() {
    let examples = examples_dir();
    let path = PathBuf::from("hosts").join("nas.kdl");
    let src = fs::read_to_string(examples.join(&path)).expect("read host kdl");
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
    )
    .expect("generate");
    for f in files {
        let basename = f.path.file_name().unwrap();
        fs::write(examples.join("expected").join(basename), &f.text).unwrap();
    }
}
```

- [ ] **Step 2: Run the writer to generate `examples/expected/nas.nix`**

Run: `KNIXL_FORMATTER=nixfmt cargo test -p knixl-pipeline --test golden write_nas_golden -- --ignored --exact`
Expected: PASS, and `examples/expected/nas.nix` now exists.

- [ ] **Step 3: Sanity-check the generated golden**

Read `examples/expected/nas.nix`. Confirm it starts with the `# Generated by knixl ...` header (like `shared.nix`), contains `networking.hostId = "8425e349";`, `services.openssh.ports = [` with `22` and `2222`, `boot.zfs.extraPools = [` with `"tank"`, and the ARC modprobe line. If anything is malformed, fix the offending manifest in Task 2 and re-run Step 2.

- [ ] **Step 4: Remove the writer test**

Delete the `write_nas_golden` test added in Step 1. The committed golden is the artefact; the writer is not kept.

- [ ] **Step 5: Add the byte-for-byte golden test**

Add to `crates/knixl-pipeline/tests/golden.rs` (mirror `web_matches_golden`):

```rust
#[test]
fn nas_matches_golden() {
    if !formatter_available() {
        eprintln!("skipping nas_matches_golden: no formatter (set KNIXL_FORMATTER)");
        return;
    }
    assert_host_matches("nas.kdl");
}
```

- [ ] **Step 6: Verify the byte golden passes**

Run: `KNIXL_FORMATTER=nixfmt cargo test -p knixl-pipeline --test golden nas_matches_golden`
Expected: PASS (byte-for-byte match).

- [ ] **Step 7: Format and run the whole workspace**

Run: `cargo fmt --all && cargo test --workspace`
Expected: PASS, tree clean. (The byte `*_matches_golden` tests skip without `KNIXL_FORMATTER`; that is expected and green.)

---

## Self-Review

- **Spec coverage:** grammar feature (Task 1) ✔; zfs/user/openssh manifests (Task 2) ✔; nas golden host + structural/attribution/byte tests (Tasks 2-3) ✔; docs/04 update (Task 1 Step 10) ✔; collect-opt empty/non-empty unit tests (Task 1) ✔. No lock/oracle change (none in plan) ✔.
- **Placeholder scan:** every code and manifest block is complete; no TBD/TODO.
- **Type consistency:** `ValueTemplate::CollectOpt(String)` is defined (Task 1 Step 3) and used consistently in parse (Step 4), interpret (Step 5), run (Step 6), dry-check (Step 7). The nas.kdl invocation syntax matches the module schemas (positional arg for single-arg repeated children).
