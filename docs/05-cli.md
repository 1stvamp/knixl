# 05: CLI and exit codes

Full sketch in `crates/knixl-cli/src/main.rs`. The design rule: `Plan::compute` is the only thing that inspects the world, every command is a thin policy over the same `Plan`, and exit codes are stable and documented so CI can branch on them.

## Finding your project

Every command discovers its project root by walking up from the current directory: the first directory holding either `knixl.lock.kdl` or a `hosts/` directory wins (`discover_root` in `crates/knixl-cli/src/main.rs`). If neither turns up before the filesystem root, the starting directory is used as-is. `modules/` is not itself part of that walk, but once a root is found it is expected to sit beside `hosts/` at that root (module lookups resolve it as `<root>/modules`), so run knixl from inside a project tree, not above or beside it.

## Commands

- `knixl plan [--detailed-exitcode]` : recompute and report, write nothing. Default exit 0; opt into scriptable codes (Terraform-style) with the flag.
- `knixl check` : CI gate. Succeed only if every file is `Clean`. Never writes, never prompts.
- `knixl generate [--accept-drift] [--prune]` : apply. Silent for `Stale`/`Missing`, refuses `Drifted` without `--accept-drift`, refuses version skew (points at `upgrade`), deletes `Orphaned` only with `--prune`.
- `knixl upgrade [--yes]` : the only path that changes recorded versions. Shows per-module migration notes and a diff, applies on `--yes`, then bumps tool/module/formatter/oracle versions together. Also resolves any host's declared `nixpkgs release="<rel>"` that has no lock entry yet, or whose lock entry is stale, writing the resolved baseline alongside the version bump (see the baseline note under `install`).
- `knixl doc <node>` : typed reference from `schema()`. For example, `knixl doc web-service`:

  ```
  web-service: Hardened nginx reverse-proxy virtual host.

  Arguments:
    host : string (required)  Virtual host name.

  Children:
    upstream : string (required)  Proxy target URL.
    acme : node
    hardened : bool  Add recommended security headers.
    alias : string (repeated)  Additional server name.
    location : node (repeated)  Extra proxied path.
  ```
- `knixl install <pkg> [--host <name>] [--yes] [--strict] [--build] [--no-abi-check]` : add a package to a host. Drafts a `package "<pkg>"` node into the host KDL (format-preserving), verifies it (generate + oracle, then a nix eval that the package resolves and the file parses). `<pkg>` can be a package name (e.g. `curl`) or a versioned form `pkg@version` (e.g. `curl@8.4.0`); a version pins the package to a specific nixpkgs commit, resolved by default via a built-in resolver that queries the nixhub/devbox version index over HTTPS (honouring `HTTP_PROXY`/`HTTPS_PROXY`/`NO_PROXY`); `KNIXL_PIN_RESOLVER` overrides it with an external `<name> <version>` -> `<commit>` command; resolution is at install time, recorded per host in the lock, and emitted from that pinned commit via `builtins.fetchGit { url; rev; }` mixed into the host baseline (a full 40-char git rev is a complete pure pin on its own, so no sha256 is needed); an unresolvable version refuses (exit 5). With `[--build]`, verification additionally builds the package derivation (`pkgs.<pkg>`) from the pinned rev; nix-absent skips unless `--strict`, and a failed build gates the apply; pairing `--build` with version pinning catches cross-rev build breakage. In the TUI this appears as a build status row per package (built once per package, not re-run on host switch); the pin state appears as a row when a version is set. Previews the change, and on confirmation regenerates through the apply path. Host order: `--host`, then the `default=#true` host, then the sole host. A nix that is not on PATH is a warning that skips the eval, unless `--strict` makes it an error; a package that does not resolve always refuses. On an interactive terminal (and without `--yes`) it opens the Install screen of the TUI (see `knixl tui`): switch the target host, edit the package, watch it verify (async, with a spinner), scroll the generated `.nix`, and apply or cancel; piped, in CI, or under `--yes` it uses the plain `[y/N]` confirm.

  A version pin resolves to one of two emit strategies (ADR 0006): `override` (`pkgs.<pkg>.overrideAttrs` with `version`/`src` taken from the pinned commit, built against the host's baseline deps; lean, tried first) or `commit-mix` (the ADR 0005 default and fallback: the whole historical package, built against its own era's deps). Selection happens automatically at pin time by build-testing `override` first and falling back to `commit-mix` only if it fails to build; `--no-abi-check` skips that build-feasibility test entirely and always takes `commit-mix`. The chosen strategy is recorded per pin in the lock, visible as `strategy="override"` on the pin line (an absent `strategy` attr means `commit-mix`).

  A host may also declare a baseline nixpkgs release: `nixpkgs release="<rel>"` as a child node of `host` (e.g. `nixpkgs release="25.05"`). It is metadata only, never emitted into the generated `.nix`. It resolves to a commit only at `install`/`upgrade` time (via the same built-in resolver, `git ls-remote` against the `nixos-<rel>` branch with a GitHub API fallback, or `KNIXL_BASELINE_RESOLVER` for an external override), and is recorded as a `baseline release="<rel>" nixpkgs-rev="<commit>" options-hash="<hash>"` line per host in the lock, beside that host's `pin` lines. That baseline drives both the host's oracle validation and its pin-strategy feasibility test. A declared release with no resolved lock entry refuses (exit 5), the same as an unresolved package pin.
- `knixl tui` : the interactive hub (bubbletea + lipgloss). Home routes to three screens: Install (as above), Browse (list registered modules built-in and declarative, read a module's schema doc, and scaffold its node into a host), and New module (a form that writes a starter `modules/<name>/knixl-module.kdl`). A movable focus selector drives every control, the layout resizes to the terminal, and it refuses to launch on a non-TTY rather than hanging.

`--json` is global for machine-readable output.

## Environment variables

| Variable | Overrides | Default when unset |
| --- | --- | --- |
| `KNIXL_FORMATTER` | The formatter binary run to format generated Nix. | Autodetected: the first of `nixfmt`, `nixfmt-rfc-style` that runs; `nixfmt` if neither does. |
| `KNIXL_NIX` | The nix evaluation binary used for the `install` package/parse checks. | `nix-instantiate` |
| `KNIXL_NIX_BUILD` | The nix build binary used for `install --build` and the pin-strategy feasibility test. | `nix-build` |
| `KNIXL_OPTIONS_JSON` | A prebuilt oracle `options.json`, used to validate every host regardless of its baseline rev. | Per host: the cached options set for that host's baseline rev (or the lock's default rev), if cached; otherwise validation is skipped for that host. |
| `KNIXL_PIN_RESOLVER` | An external `<bin> <name> <version>` command that resolves a package pin to a nixpkgs commit. | The built-in resolver: queries the nixhub/devbox version index over HTTPS. |
| `KNIXL_BASELINE_RESOLVER` | An external `<bin> <release>` command that resolves a host's declared `nixpkgs release` to a commit. | The built-in resolver: `git ls-remote` against the `nixos-<release>` branch, falling back to the GitHub commits API if `git` is unavailable or fails. |
| `HTTP_PROXY`, `HTTPS_PROXY`, `NO_PROXY` | Proxying for the built-in pin and baseline resolvers' HTTPS requests. | No proxy. |

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
