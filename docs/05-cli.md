# 05: CLI and exit codes

Full sketch in `crates/knixl-cli/src/main.rs`. The design rule: `Plan::compute` is the only thing that inspects the world, every command is a thin policy over the same `Plan`, and exit codes are stable and documented so CI can branch on them.

## Commands

- `knixl plan [--detailed-exitcode]` : recompute and report, write nothing. Default exit 0; opt into scriptable codes (Terraform-style) with the flag.
- `knixl check` : CI gate. Succeed only if every file is `Clean`. Never writes, never prompts.
- `knixl generate [--accept-drift] [--prune]` : apply. Silent for `Stale`/`Missing`, refuses `Drifted` without `--accept-drift`, refuses version skew (points at `upgrade`), deletes `Orphaned` only with `--prune`.
- `knixl upgrade [--yes]` : the only path that changes recorded versions. Shows per-module migration notes and a diff, applies on `--yes`, then bumps tool/module/formatter/oracle versions together.
- `knixl doc <node>` : typed reference from `schema()`.
- `knixl install <pkg> [--host <name>] [--yes] [--strict] [--build]` : add a package to a host. Drafts a `package "<pkg>"` node into the host KDL (format-preserving), verifies it (generate + oracle, then a nix eval that the package resolves and the file parses). `<pkg>` can be a package name (e.g. `curl`) or a versioned form `pkg@version` (e.g. `curl@8.4.0`); a version pins the package to a specific nixpkgs commit, resolved by default via a built-in resolver that queries the nixhub/devbox version index over HTTPS (honouring `HTTP_PROXY`/`HTTPS_PROXY`/`NO_PROXY`) and prefetches the sha with `nix` (so `nix` must be present); `KNIXL_PIN_RESOLVER` overrides it with an external `<name> <version>` -> `<commit> <sha256>` command; resolution is at install time, recorded per host in the lock, and emitted from that pinned commit mixed into the host baseline; an unresolvable version refuses (exit 5). With `[--build]`, verification additionally builds the package derivation (`pkgs.<pkg>`) from the pinned rev; nix-absent skips unless `--strict`, and a failed build gates the apply; pairing `--build` with version pinning catches cross-rev build breakage. In the TUI this appears as a build status row per package (built once per package, not re-run on host switch); the pin state appears as a row when a version is set. Previews the change, and on confirmation regenerates through the apply path. Host order: `--host`, then the `default=#true` host, then the sole host. A nix that is not on PATH is a warning that skips the eval, unless `--strict` makes it an error; a package that does not resolve always refuses. On an interactive terminal (and without `--yes`) it opens the Install screen of the TUI (see `knixl tui`): switch the target host, edit the package, watch it verify (async, with a spinner), scroll the generated `.nix`, and apply or cancel; piped, in CI, or under `--yes` it uses the plain `[y/N]` confirm.
- `knixl tui` : the interactive hub (bubbletea + lipgloss). Home routes to three screens: Install (as above), Browse (list registered modules built-in and declarative, read a module's schema doc, and scaffold its node into a host), and New module (a form that writes a starter `modules/<name>/knixl-module.kdl`). A movable focus selector drives every control, the layout resizes to the terminal, and it refuses to launch on a non-TTY rather than hanging.

`--json` is global for machine-readable output.

## Exit codes

One enum, precedence spelled out (severity order is not numeric order):

- `0 Clean` : nothing to do, or applied cleanly.
- `1 Internal` : a panic turned into an error.
- `2 Usage` : clap default.
- `3 Drift` : a generated file was hand-edited (tainted).
- `4 NeedsAck` : version skew would change output; needs `upgrade` or `--yes`.
- `5 Validation` : KDL schema error or oracle type / unknown-option error.
- `6 RegenPending` : `Stale`/`Missing`/`Orphaned`; inputs changed, regeneration owed.

Precedence, most severe first: `Validation` beats everything (you cannot trust a plan built on invalid input). `Drift` beats skew (silent overwrite would lose human edits). Skew (`NeedsAck`) beats plain `RegenPending` (a version bump is a bigger claim than an input edit).

```rust
fn verdict(plan: &Plan) -> Code {
    if plan.has_validation_errors() { return Code::Validation; }
    if plan.any(FileState::is_drifted) { return Code::Drift; }
    if plan.requires_ack()            { return Code::NeedsAck; }
    if plan.any(FileState::is_dirty)  { return Code::RegenPending; }
    Code::Clean
}
```

## How the commands map policy over the plan

- `check` : print the plan, return `verdict`. Nothing else. This is the line you put in CI.
- `generate` : refuse outright if `plan.requires_ack()` (skew must go through `upgrade`). Otherwise apply `Stale`/`Missing`, apply `Drifted` only under `--accept-drift` (retaking the hash), leave other `Drifted` as exit 3, delete `Orphaned` only under `--prune`. Commit the lock only on a fully clean apply, so a partial or refused run never leaves the recorded hashes lying about what is on disk.
- `upgrade` : print migration notes keyed by `(module, version delta)`, print the plan, require `--yes`, then write files and bump every version in the lock together.

## How this satisfies the original requirements

- **Immutable recreation:** `knixl check` recomputes expected output from committed KDL + locked versions, hashes it, exits 0 only if every file is byte-identical. Safe to run anywhere (no writes).
- **No silent regressions on upgrade:** the `generate`/`upgrade` split. `generate` refuses skew (exit 4), `upgrade` forces a human past notes and diff before rewriting versions.
- **Taint:** the `Drifted` state and exit 3. Detected by the third hash. Forward paths are reconcile-to-KDL or an explicit `--accept-drift` that knowingly discards the edit.
