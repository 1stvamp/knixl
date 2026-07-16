# install --build (slice B) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an opt-in `knixl install --build` that builds the package derivation (`pkgs.<pkg>`) to prove it builds, not just resolves + parses.

**Architecture:** A new `NixEval::builds` shells out to `nix-build`. The build is package-only and host-independent, run once per package: in the TUI as an async status row (spinner, gates Apply), and in the plain path before the `[y/N]` confirm. With `--build` absent, every path is byte-for-byte unchanged.

**Tech Stack:** Rust workspace (`knixl-nix`, `knixl-cli`), bubbletea-rs + bubbletea-widgets (spinner) + lipgloss for the TUI, clap for the CLI.

## Global Constraints

- British spelling in all prose, comments, docstrings. No em-dashes or en-dashes: use colons, parentheses, commas, full stops.
- Banned vocabulary (docs, comments, commit messages): passionate, leverage, robust, seamless, delve, and the AI-smell set.
- Deterministic emit is unaffected here, but keep no `HashMap` in any emit path (not touched by this plan).
- Commit only when a task's tests pass. Use GitButler: `but commit feat/install-build -c -m "<msg>" --changes <ids>` (get ids from `but status`). Branch `feat/install-build` already exists (holds the spec commit).
- MSRV 1.87, kdl 6.5 pin unchanged. Do not add dependencies (spinner/textinput/viewport already present).
- Injection pattern: nix work is injected behind `Send + Sync` closures resolved from the project root, closing over only `Send` data (mirror `make_verify` in `crates/knixl-cli/src/main.rs`).

---

### Task 1: `NixEval::builds` + build binary (knixl-nix)

**Files:**
- Modify: `crates/knixl-nix/src/nixeval.rs` (struct `NixEval`, `resolve`, new `builds`, tests)

**Interfaces:**
- Consumes: `Nixpkgs` (existing), `NixError` (existing), `output_retrying_etxtbsy` (existing, `crate::`).
- Produces: `NixEval { pub bin: PathBuf, pub build_bin: PathBuf }`; `pub fn builds(&self, src: &Nixpkgs, name: &str) -> Result<(), NixError>`.

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `crates/knixl-nix/src/nixeval.rs`. The existing `shim` helper writes a `nix-instantiate` stand-in; add a build shim that exits based on a flag.

```rust
/// A shim mimicking `nix-build`: exits 0 when `build_ok`, else 1 with a message on stderr.
fn build_shim(tag: &str, build_ok: bool) -> PathBuf {
    let path = std::env::temp_dir().join(format!("knixl-buildshim-{}-{tag}", std::process::id()));
    let exit = if build_ok { 0 } else { 1 };
    let script = format!("#!/bin/sh\necho 'boom' 1>&2\nexit {exit}\n");
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(script.as_bytes()).unwrap();
    f.flush().unwrap();
    drop(f);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[test]
fn builds_ok_when_shim_exits_zero() {
    let e = NixEval { bin: PathBuf::from("nix-instantiate"), build_bin: build_shim("bok", true) };
    assert!(e.builds(&Nixpkgs::Ambient, "ripgrep").is_ok());
}

#[test]
fn builds_failed_when_shim_exits_nonzero() {
    let e = NixEval { bin: PathBuf::from("nix-instantiate"), build_bin: build_shim("bbad", false) };
    assert!(matches!(e.builds(&Nixpkgs::Ambient, "ripgrep"), Err(NixError::Failed(_))));
}

#[test]
fn builds_unavailable_when_binary_missing() {
    let e = NixEval {
        bin: PathBuf::from("nix-instantiate"),
        build_bin: PathBuf::from("/nonexistent/knixl-no-such-nix-build"),
    };
    assert!(matches!(e.builds(&Nixpkgs::Ambient, "x"), Err(NixError::Unavailable(_))));
}
```

Note: the existing tests construct `NixEval { bin: ... }`. After Step 3 they must become `NixEval { bin: ..., build_bin: PathBuf::from("nix-build") }`. Update each existing `NixEval { bin: shim(...) }` literal in this file to add `build_bin: PathBuf::from("nix-build")`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p knixl-nix builds_ 2>&1 | tail`
Expected: FAIL to compile (`build_bin` field and `builds` method do not exist).

- [ ] **Step 3: Add the field, resolve it, and implement `builds`**

In `crates/knixl-nix/src/nixeval.rs`, change the struct and `resolve`:

```rust
/// A handle to the nix binaries. `KNIXL_NIX` overrides the eval binary and
/// `KNIXL_NIX_BUILD` the build binary (shims in tests).
#[derive(Debug, Clone)]
pub struct NixEval {
    pub bin: PathBuf,
    pub build_bin: PathBuf,
}

impl NixEval {
    /// Resolve the checkers: `KNIXL_NIX` (else `nix-instantiate`) and `KNIXL_NIX_BUILD`
    /// (else `nix-build`).
    pub fn resolve() -> NixEval {
        let bin = std::env::var_os("KNIXL_NIX")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("nix-instantiate"));
        let build_bin = std::env::var_os("KNIXL_NIX_BUILD")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("nix-build"));
        NixEval { bin, build_bin }
    }
    // ... existing run/package_exists/parses unchanged ...
}
```

Add the `builds` method (after `parses`):

```rust
/// Build `pkgs.<name>` from the given nixpkgs, proving the package derivation builds.
/// `--no-out-link` avoids leaving a `result` symlink.
pub fn builds(&self, src: &Nixpkgs, name: &str) -> Result<(), NixError> {
    let expr = src.expr();
    let out = crate::output_retrying_etxtbsy(|| {
        let mut c = Command::new(&self.build_bin);
        c.args(["--no-out-link", "-A", name, "-E", &expr]);
        c
    })
    .map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            NixError::Unavailable(format!("{} not found", self.build_bin.display()))
        } else {
            NixError::Unavailable(e.to_string())
        }
    })?;
    if out.status.success() {
        Ok(())
    } else {
        Err(NixError::Failed(String::from_utf8_lossy(&out.stderr).trim().to_string()))
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p knixl-nix 2>&1 | tail`
Expected: PASS (all existing + 3 new). Then `cargo clippy -p knixl-nix` clean.

- [ ] **Step 5: Commit**

`but status` to get change ids, then:
`but commit feat/install-build -c -m "feat(nix): NixEval::builds package derivation via nix-build" --changes <ids>`

---

### Task 2: TUI build plumbing (types, Entry, config)

**Files:**
- Modify: `crates/knixl-cli/src/tui/mod.rs` (add `BuildOutcome`, `BuildFn`; extend `Entry::Install`, `TuiConfig`, `run`)
- Modify: `crates/knixl-cli/src/main.rs` (callers of `Entry::Install` and `hub::run` compile again)

**Interfaces:**
- Produces:
  - `pub enum BuildOutcome { Ok, Failed, Skipped }` (Debug, Clone, Copy, PartialEq, Eq)
  - `pub type BuildFn = Arc<dyn Fn(&str) -> BuildOutcome + Send + Sync>`
  - `Entry::Install { pkg: String, strict: bool, host: Option<String>, build: bool }`
  - `TuiConfig { ..., pub build: Option<BuildFn> }`
  - `pub fn run(entry, root, hosts, verify, modules, build: Option<BuildFn>) -> Result<Outcome, String>`
- Consumes: `Arc` (already imported), `HostInfo`.

This task is compile-only plumbing: no behaviour change yet (the Install screen ignores `build` until Task 3; callers pass `false`/`None`).

- [ ] **Step 1: Add the types to `crates/knixl-cli/src/tui/mod.rs`**

After the `VerifyFn` definition, add:

```rust
/// The result of the async package build (`--build`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildOutcome {
    Ok,
    Failed,
    Skipped,
}

/// Builds `pkgs.<pkg>` (host-independent). Injected only when `--build` is requested;
/// `Send + Sync` so the Install screen runs it off the event loop.
pub type BuildFn = Arc<dyn Fn(&str) -> BuildOutcome + Send + Sync>;
```

Extend `Entry::Install`:

```rust
    Install { pkg: String, strict: bool, host: Option<String>, build: bool },
```

Extend `TuiConfig` (add field) and `run` (add param, set field):

```rust
pub struct TuiConfig {
    #[allow(dead_code)]
    pub root: PathBuf,
    pub hosts: Vec<HostInfo>,
    pub entry: Entry,
    pub verify: VerifyFn,
    pub modules: Vec<BrowseModule>,
    pub build: Option<BuildFn>,
}

pub fn run(
    entry: Entry,
    root: PathBuf,
    hosts: Vec<HostInfo>,
    verify: VerifyFn,
    modules: Vec<BrowseModule>,
    build: Option<BuildFn>,
) -> Result<Outcome, String> {
    let _ = CONFIG.set(TuiConfig { root, hosts, entry, verify, modules, build });
    // ... rest unchanged ...
}
```

- [ ] **Step 2: Fix the callers in `crates/knixl-cli/src/main.rs`**

In `open_tui`, pass the new arg (None for now):
`hub::run(entry, root.clone(), hosts, make_verify(root), modules, None)`

In `install`, the interactive `Entry::Install { pkg, strict, host }` literal gains `build: false` for now (Task 4 wires the real flag):
`hub::Entry::Install { pkg: pkg.to_string(), strict, host: Some(initial.name.clone()), build: false }`

Any test in `tui/mod.rs` constructing `Entry::Install` (none currently) would also need the field.

- [ ] **Step 3: Build to verify it compiles**

Run: `cargo build -p knixl-cli 2>&1 | tail`
Expected: Finished, no errors. `cargo test -p knixl-cli 2>&1 | tail` still green (no behaviour change).

- [ ] **Step 4: Commit**

`but commit feat/install-build -c -m "feat(tui): build-plumbing types and config for install --build" --changes <ids>`

---

### Task 3: Install screen build status, gating, and view

**Files:**
- Modify: `crates/knixl-cli/src/tui/install.rs` (state, reducers, view, async build cmd, tests)

**Interfaces:**
- Consumes: `super::{config, BuildOutcome}`, `bubbletea_widgets::spinner`, existing `new_spinner`, `spin_start`.
- Produces: internal `BuildState`, `begin_build`, `on_build_done`, extended `apply_allowed`.

**Design recap:** a build row shows only when `--build` requested. Build runs on entry and on package-edit Enter (not host switch). Its own spinner + `build_seq` token; apply gated on it.

- [ ] **Step 1: Write the failing reducer tests**

Add to the `tests` module in `crates/knixl-cli/src/tui/install.rs`. The `model(hosts)` builder must be updated in Step 3 to set the new fields; write tests against the methods they exercise.

```rust
#[test]
fn build_gating_blocks_apply_until_it_succeeds() {
    let mut m = model(1);
    m.build = BuildState::Building;   // requested + in flight
    assert!(!m.apply_allowed(), "in-flight build blocks apply");
    m.build = BuildState::Failed;
    assert!(!m.apply_allowed(), "failed build blocks apply");
    m.build = BuildState::Ok;
    assert!(m.apply_allowed(), "successful build allows apply");
}

#[test]
fn build_skipped_gates_only_under_strict() {
    let mut m = model(1);
    m.build = BuildState::Skipped;
    m.strict = false;
    assert!(m.apply_allowed(), "skipped build is fine without --strict");
    m.strict = true;
    assert!(!m.apply_allowed(), "--strict rejects a skipped build");
}

#[test]
fn build_off_does_not_affect_gating() {
    let mut m = model(1);       // resolves Yes, parses Ok by default
    m.build = BuildState::Off;
    assert!(m.apply_allowed(), "no --build means the build never gates");
}

#[test]
fn on_build_done_sets_state_and_ignores_stale() {
    let mut m = model(1);
    m.build = BuildState::Building;
    let seq = m.mark_building();       // bumps build_seq, sets Building
    m.on_build_done(seq, BuildOutcome::Ok);
    assert_eq!(m.build, BuildState::Ok);
    let stale = seq;                   // superseded
    m.mark_building();
    m.on_build_done(stale, BuildOutcome::Failed);
    assert_eq!(m.build, BuildState::Building, "stale build result discarded");
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p knixl-cli build_ 2>&1 | tail`
Expected: FAIL to compile (`BuildState`, `m.build`, `mark_building`, `on_build_done` do not exist).

- [ ] **Step 3: Implement the build state and reducers**

Add near the top of `crates/knixl-cli/src/tui/install.rs`:

```rust
/// The `--build` status. `Off` means `--build` was not requested (never gates apply).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BuildState {
    Off,
    Building,
    Ok,
    Failed,
    Skipped,
}

/// The async build result, delivered back to `update`.
struct BuildDone {
    seq: u64,
    outcome: super::BuildOutcome,
}
```

Add fields to `InstallModel` (after `spinner`):

```rust
    build: BuildState,
    build_spinner: spinner::Model,
    build_seq: u64,
```

In `enter`, initialise them: `build` is `BuildState::Building` if `config().build.is_some()` else `BuildState::Off`; `build_spinner: new_spinner()`; `build_seq: 0`. Then, after `let cmd = model.begin_verify();`, batch in the build command:

```rust
        let verify = model.begin_verify();
        let build = model.begin_build();
        let cmd = match (verify, build) {
            (Some(v), Some(b)) => Some(command::batch(vec![v, b])),
            (v, None) => v,
            (None, b) => b,
        };
        (model, cmd)
```

Add the reducers (near `mark_verifying` / `on_verify_done`):

```rust
    fn mark_building(&mut self) -> u64 {
        self.build_seq += 1;
        self.build = BuildState::Building;
        self.build_seq
    }

    /// Start a build for the current package, if `--build` was requested. Host-independent,
    /// so this is NOT called on a host switch.
    fn begin_build(&mut self) -> Option<Cmd> {
        config().build.as_ref()?; // None => --build not requested
        let seq = self.mark_building();
        let pkg = self.pkg.value();
        Some(command::batch(vec![build_cmd(seq, pkg), spin_start(self.build_spinner.tick_msg())]))
    }

    fn on_build_done(&mut self, seq: u64, outcome: super::BuildOutcome) {
        if seq != self.build_seq {
            return;
        }
        self.build = match outcome {
            super::BuildOutcome::Ok => BuildState::Ok,
            super::BuildOutcome::Failed => BuildState::Failed,
            super::BuildOutcome::Skipped => BuildState::Skipped,
        };
    }
```

Extend `apply_allowed` (add before the final `resolve_ok && parse_ok`):

```rust
        let build_ok = match self.build {
            BuildState::Off | BuildState::Ok => true,
            BuildState::Skipped => !self.strict,
            BuildState::Building | BuildState::Failed => false,
        };
        resolve_ok && parse_ok && build_ok
```

Add the build command and update `begin_verify` to re-run the build on a package edit. In the `Focus::Package` `KeyCode::Enter` arm of `update`, batch verify + build:

```rust
                KeyCode::Enter => {
                    let v = self.begin_verify();
                    let b = self.begin_build();
                    let cmd = match (v, b) {
                        (Some(v), Some(b)) => Some(command::batch(vec![v, b])),
                        (v, None) => v,
                        (None, b) => b,
                    };
                    Step { nav: Nav::Stay, cmd }
                }
```

Add message handling in `update` (next to the `VerifyDone` / spinner arms):

```rust
        if let Some(done) = msg.downcast_ref::<BuildDone>() {
            self.on_build_done(done.seq, done.outcome);
            return Step::stay();
        }
        if msg.downcast_ref::<spinner::TickMsg>().is_some() {
            // Advance whichever spinner is animating; re-arm only while its work is live.
            let vcmd = if self.verifying { self.spinner.update(clone_tick(&msg)) } else { None };
            let bcmd = if self.build == BuildState::Building {
                self.build_spinner.update(msg)
            } else {
                None
            };
            let cmd = match (vcmd, bcmd) {
                (Some(a), Some(b)) => Some(command::batch(vec![a, b])),
                (a, None) => a,
                (None, b) => b,
            };
            return Step { nav: Nav::Stay, cmd };
        }
```

Note: the two spinners share the `spinner::TickMsg` type but have distinct ids; each `update` ignores a foreign id. Since a `Msg` is consumed by `update`, feed each spinner its own tick. Simplest correct approach: give the build its own tick message type is overkill; instead, forward the single `TickMsg` to both spinners by value is impossible (Msg is not Clone). Replace the block above with: forward the owned `msg` to whichever spinner matches its id, by checking the tick id against each spinner's id:

```rust
        if let Some(tick) = msg.downcast_ref::<spinner::TickMsg>() {
            if tick.id == self.build_spinner.id() {
                let cmd = self.build_spinner.update(msg);
                return Step { nav: Nav::Stay, cmd: (self.build == BuildState::Building).then_some(cmd).flatten() };
            }
            let cmd = self.spinner.update(msg);
            return Step { nav: Nav::Stay, cmd: if self.verifying { cmd } else { None } };
        }
```

(Use this second form; delete the first. It routes each tick to the right spinner by id, keeping both animations independent.)

Add the build command function (next to `verify_cmd`):

```rust
/// The package build, off the event-loop thread. Resolves to a `BuildDone` with the token
/// so a stale build (package edited again) is discarded. Only called when `config().build`
/// is `Some`.
fn build_cmd(seq: u64, pkg: String) -> Cmd {
    let build = config().build.clone().expect("build fn present when begin_build ran");
    Box::pin(async move {
        match tokio::task::spawn_blocking(move || build(&pkg)).await {
            Ok(outcome) => Some(Box::new(BuildDone { seq, outcome }) as Msg),
            Err(_) => None,
        }
    })
}
```

Update the test `model(hosts)` builder to set the new fields: `build: BuildState::Off, build_spinner: new_spinner(), build_seq: 0`.

- [ ] **Step 4: Add the build row to `view`**

In `view`, build a `build_line` only when `self.build != BuildState::Off`, and insert it into the `lines` array right after `verify_line`:

```rust
        let build_line = (self.build != BuildState::Off).then(|| {
            let status = match self.build {
                BuildState::Building => format!("{} building", self.build_spinner.view()),
                BuildState::Ok => theme::good().render("\u{2713} builds"),
                BuildState::Failed => theme::bad().render("\u{2717} build failed"),
                BuildState::Skipped => theme::amber().render("\u{00b7} build skipped"),
                BuildState::Off => String::new(),
            };
            format!("{}{}{}", marker(false), theme::dim().render("build  "), status)
        });
```

Change the `lines` array to a `Vec<String>` so the row is conditional:

```rust
        let mut lines = vec![theme::chip(" install "), host_line, pkg_line, strict_line, verify_line];
        if let Some(b) = build_line { lines.push(b); }
        lines.extend([preview_hdr, preview_box, buttons, hint]);
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        join_vertical(LEFT, &refs)
```

- [ ] **Step 5: Run tests + clippy**

Run: `cargo test -p knixl-cli 2>&1 | tail` (all pass, including the 4 new build tests)
Run: `cargo clippy -p knixl-cli --all-targets 2>&1 | grep -cE 'warning:|error'` (expect 0)

- [ ] **Step 6: Commit**

`but commit feat/install-build -c -m "feat(tui): install --build status row, spinner, and apply gating" --changes <ids>`

---

### Task 4: CLI wiring (`--build` flag, BuildFn, plain path)

**Files:**
- Modify: `crates/knixl-cli/src/main.rs` (`Cmd::Install`, `install`, `open_tui`, new `make_build`, plain-path build)
- Modify: `crates/knixl-cli/tests/cli.rs` (integration test)

**Interfaces:**
- Consumes: `hub::{BuildFn, BuildOutcome, Entry}`, `knixl_nix::nixeval::{NixEval, NixError, Nixpkgs}`, `resolve_package_rev`, `default_formatter`, `gather::gather`.
- Produces: `fn make_build(root: PathBuf) -> hub::BuildFn`; `install(..., build: bool)`.

- [ ] **Step 1: Add the flag and thread it**

In `crates/knixl-cli/src/main.rs`, `Cmd::Install`:

```rust
    Install {
        pkg: String,
        #[arg(long)] host: Option<String>,
        #[arg(long)] yes: bool,
        #[arg(long)] strict: bool,
        /// Also build the package derivation (proves it builds, not just resolves).
        #[arg(long)] build: bool,
    },
```

Update the dispatch arm:
`Cmd::Install { pkg, host, yes, strict, build } => install(ctx, &pkg, host.as_deref(), yes, strict, build),`

- [ ] **Step 2: Write the failing CLI integration test**

In `crates/knixl-cli/tests/cli.rs`, mirror the existing install tests (they set `KNIXL_NIX` to a shim). Add a build-failure test that sets `KNIXL_NIX_BUILD` to a failing shim and asserts refusal. Use the same harness helpers the file already provides (a temp project + `knixl` command). Concretely:

```rust
#[test]
fn install_build_refuses_when_the_build_fails() {
    let proj = sample_project();           // existing helper that scaffolds a host
    let ok_eval = write_shim(&proj, "eval", "true", true);   // package resolves + parses
    let bad_build = write_build_shim(&proj, "build", false);  // build exits non-zero
    let out = knixl(&proj)
        .args(["install", "ripgrep", "--yes", "--build"])
        .env("KNIXL_NIX", &ok_eval)
        .env("KNIXL_NIX_BUILD", &bad_build)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(5), "build failure exits Validation (5): {out:?}");
}
```

If `write_build_shim`/`sample_project`/`knixl`/`write_shim` do not exist under these exact names, reuse the file's existing equivalents (read `crates/knixl-cli/tests/cli.rs` first and match its helper names and shim-writing style; add a `write_build_shim` that writes `#!/bin/sh\nexit 1\n` if there is no build-shim helper).

- [ ] **Step 3: Run to verify it fails**

Run: `cargo test -p knixl-cli --test cli install_build_ 2>&1 | tail`
Expected: FAIL (flag/behaviour not implemented, or compile error on missing helper — add the helper, then it should fail on the assertion).

- [ ] **Step 4: Implement `make_build`, the plain-path build, and pass the BuildFn**

Add `make_build` next to `make_verify`:

```rust
/// The build function for the Install screen: builds `pkgs.<pkg>` from the lock's pinned rev
/// (ambient fallback), mapping nix errors to a coarse outcome. Closes over only `root`.
fn make_build(root: std::path::PathBuf) -> hub::BuildFn {
    use knixl_nix::nixeval::{NixError, NixEval, Nixpkgs};
    std::sync::Arc::new(move |pkg: &str| {
        let rev = read_pinned_rev(&root);
        let src = if rev.is_empty() { Nixpkgs::Ambient } else { Nixpkgs::PinnedRev(rev) };
        match NixEval::resolve().builds(&src, pkg) {
            Ok(()) => hub::BuildOutcome::Ok,
            Err(NixError::Unavailable(_)) => hub::BuildOutcome::Skipped,
            Err(NixError::Failed(_)) => hub::BuildOutcome::Failed,
        }
    })
}

/// The lock's pinned nixpkgs rev for `root`, or empty if unavailable.
fn read_pinned_rev(root: &std::path::Path) -> String {
    let formatter = default_formatter();
    let tool: semver::Version = env!("CARGO_PKG_VERSION").parse().expect("tool version parses");
    knixl_pipeline::gather::gather(root, &formatter, tool)
        .map(|p| p.lock.oracle.nixpkgs_rev)
        .unwrap_or_default()
}
```

Change `open_tui` to accept and pass a build fn:

```rust
fn open_tui(entry: hub::Entry, build: Option<hub::BuildFn>) -> Result<hub::Outcome, String> {
    use knixl_pipeline::install::list_hosts;
    let root = discover_root();
    let hosts = list_hosts(&root).map_err(|e| e.to_string())?;
    let modules = browse_modules(&root);
    hub::run(entry, root.clone(), hosts, make_verify(root.clone()), modules, build)
}
```

Update its two callers:
- `dispatch` (Tui): `open_tui(hub::Entry::Hub, None)`
- `install` interactive branch (Step 5 below).

Update `install`'s signature and body:

```rust
fn install(ctx: &Ctx, pkg: &str, host: Option<&str>, yes: bool, strict: bool, build: bool) -> Code {
    // ... list_hosts / select_host unchanged ...
    let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    if interactive && !yes {
        let entry = hub::Entry::Install {
            pkg: pkg.to_string(),
            strict,
            host: Some(initial.name.clone()),
            build,
        };
        let build_fn = build.then(|| make_build(ctx.root.clone()));
        return match open_tui(entry, build_fn) {
            Ok(hub::Outcome::Install { host, pkg, strict }) => commit_install(&host, &pkg, strict),
            Ok(_) => { println!("cancelled"); Code::Clean }
            Err(e) => { eprintln!("knixl: tui: {e}"); Code::Internal }
        };
    }

    // Plain path: resolve check (unchanged), then the build when requested, then confirm.
    match resolve_package(ctx, pkg) { /* ... unchanged match arms ... */ }
    if build {
        use knixl_nix::nixeval::{NixError, NixEval, Nixpkgs};
        let rev = &ctx.lock.oracle.nixpkgs_rev;
        let src = if rev.is_empty() { Nixpkgs::Ambient } else { Nixpkgs::PinnedRev(rev.clone()) };
        match NixEval::resolve().builds(&src, pkg) {
            Ok(()) => {}
            Err(NixError::Unavailable(_)) if strict => {
                eprintln!("knixl: --strict: nix unavailable, cannot build `{pkg}`");
                return Code::Validation;
            }
            Err(NixError::Unavailable(_)) => {
                eprintln!("warning: nix unavailable, skipping build of `{pkg}`");
            }
            Err(NixError::Failed(m)) => {
                eprintln!("knixl: `{pkg}` failed to build: {m}");
                return Code::Validation;
            }
        }
    }
    if !yes && !confirm(&format!("install {pkg} on {}?", initial.name)) {
        println!("cancelled");
        return Code::Clean;
    }
    commit_install(&initial, pkg, strict)
}
```

- [ ] **Step 5: Run tests + clippy**

Run: `cargo test -p knixl-cli 2>&1 | tail` (all pass incl. the new CLI test)
Run: `cargo clippy -p knixl-cli --all-targets 2>&1 | grep -cE 'warning:|error'` (expect 0)

- [ ] **Step 6: Commit**

`but commit feat/install-build -c -m "feat(install): --build flag, injected build fn, plain-path build gate" --changes <ids>`

---

### Task 5: docs + full-suite verification

**Files:**
- Modify: `docs/05-cli.md` (document `--build`)

- [ ] **Step 1: Document the flag**

In `docs/05-cli.md`, extend the `knixl install` bullet to mention `[--build]`: after the eval verification sentence, add that `--build` additionally builds the package derivation (`pkgs.<pkg>`) from the pinned rev, gating apply on it; nix-absent skips unless `--strict`; in the TUI it shows a build status row (built once per package, not re-run on host switch).

- [ ] **Step 2: Full workspace suite + clippy**

Run: `cargo test --workspace 2>&1 | grep -cE 'FAILED'` (expect 0)
Run: `cargo clippy --workspace --all-targets 2>&1 | grep -cE 'warning:|error'` (expect 0)

- [ ] **Step 3: Smoke test the flag surface**

Run: `./target/debug/knixl install --help 2>&1 | grep -- --build` (shows the flag)
Run (non-TTY, no nix): `echo | KNIXL_NIX_BUILD=/nonexistent ./target/debug/knixl install ripgrep --yes --build` in a project dir; expect the "nix unavailable, skipping build" warning (skip, not failure) unless `--strict`.

- [ ] **Step 4: Commit**

`but commit feat/install-build -c -m "docs(cli): document knixl install --build (slice B)" --changes <ids>`

---

## Self-review notes

- Spec coverage: build mechanism (Task 1), verification model + gating (Task 3), TUI status row (Task 3), plain path (Task 4), wiring (Tasks 2+4), testing (each task), docs (Task 5). All covered.
- The build re-runs on package edit (Task 3 Step 3, Package Enter arm) and on entry, not on host switch (host switch calls only `begin_verify`). Matches the spec.
- `--build` absent: `BuildState::Off`, no build fn injected, no build row, gating unchanged. Matches the spec's byte-for-byte claim.
- Types are consistent: `BuildOutcome` (Ok/Failed/Skipped) in `mod.rs`; `BuildState` (Off/Building/Ok/Failed/Skipped) internal to the screen; `on_build_done` maps one to the other.
