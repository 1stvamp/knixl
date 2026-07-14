# knixl install (Slice A) design

Date: 2026-07-14
Status: approved, ready for implementation plan

## Problem and goal

An apt/nala-style `knixl install <pkg>` that adds a package to a host, drafts the
KDL edit, verifies it works under nix, previews the change, and on confirmation
writes the KDL and regenerates. This is Slice A of a larger idea; it deliberately
excludes the TUI, fuzzy search, version pinning, and derivation builds (see Out of
scope). Those are later slices.

## Scope

In:

- A new built-in `package` module: `package "<name>"`, repeated under a host, lowering
  to `environment.systemPackages`.
- Pipeline list-merge so repeated `package` nodes become one `environment.systemPackages`.
- A `knixl install <pkg>` command: resolve host, draft the KDL edit, verify (knixl
  generate + oracle, then a nix eval), preview diffs, confirm, write and regenerate.
- A `default` host flag to pick the target when several hosts exist.
- A `--strict` flag that turns a skipped verification into an error.

Out (later slices): the TUI, fuzzy package search and suggestions, version pinning,
`nix build` of the derivation, and targets other than `environment.systemPackages`
(services, `programs.*`).

## Command

```
knixl install <pkg> [--host <name>] [--yes] [--strict]
```

Flow:

1. Resolve the target host (see Host selection).
2. Draft: splice `package "<pkg>"` into that host's KDL, format-preserving. If the host
   already has that package, `install` is a no-op with a note and exits clean.
3. Verify the drafted world (see Verification).
4. Preview: print the KDL edit and the resulting `.nix` diff.
5. Confirm (skipped under `--yes`), then write the KDL and regenerate through the
   existing generate apply path (write `.nix`, update the lock).

`install` reuses `gather`, `Plan::compute`, and the write path. The new surface is the
command, the drafting step, and the nix eval check.

## Host selection

Resolution order:

1. `--host <name>` if given (error if that host does not exist).
2. Otherwise the host marked `default = true` in its KDL. Exactly one default is
   allowed; two or more defaults is an error.
3. Otherwise, if there is exactly one host, that host.
4. Otherwise an error listing the available hosts and asking for `--host` or a default.

The `default` flag is a boolean prop on the `host` node (`host "web" default=#true`). It
is tooling metadata: the `host` schema accepts it, and `host` lowering ignores it, so it
emits nothing and does not affect generated output.

## The `package` module and list-merge

A new built-in `package` module:

- node name `package`, one positional string arg (the nixpkgs attribute name), repeated.
- lowers to `environment.systemPackages = [ pkgs.<name> ]` (a list with one
  `Select(Ref("pkgs"), ["<name>"])`).

Because several `package` nodes then assign the same path in one file, the pipeline
gains a list-merge step: assignments to the same option path whose values are all lists
are concatenated in source order into one assignment. This mirrors NixOS list-option
merge semantics, so `package "ripgrep"` and `package "htop"` become a single
`environment.systemPackages = [ pkgs.ripgrep pkgs.htop ]`.

Merge runs before the value-conflict lint, so merged lists do not warn. Non-list
duplicate paths still conflict as before. Merge is order-preserving and therefore
deterministic. List items here are `Select` expressions, which the let-hoisting pass
does not touch, so the two passes do not interact.

## KDL splicing

Use the `kdl` crate's format-preserving document model. Parse the host file, find the
`host` node's children block, append a `package "<name>"` node indented to match, and
re-serialise. Untouched nodes keep their exact bytes, including comments. The splice is
idempotent: if `package "<name>"` already exists under that host, do nothing.

## Verification

Against the drafted world, in order:

1. knixl `generate` + oracle: the drafted config still generates, and every option path
   validates (this already covers `environment.systemPackages` existing and typing).
2. nix eval, using the lock's `oracle nixpkgs-rev` (fall back to ambient `<nixpkgs>` when
   the lock has no rev):
   - `pkgs.<pkg>` resolves at that rev.
   - the generated module evaluates under a minimal `config`/`lib`/`pkgs`.

Outcomes:

- Verification passes: proceed to preview and write.
- Verification fails (package does not resolve, or the module fails to eval): error and
  refuse to write. `install` never commits a package it could not resolve.
- nix is not on PATH: by default, skip the eval with a loud warning and proceed (knixl's
  established best-effort pattern, as with the formatter and oracle). Under `--strict`,
  a skipped verification is an error instead.

The nix eval is behind an injectable checker (like the formatter), so tests run without
nix; real eval is verified in the VM.

## Apply and exit codes

On confirmation, `install` writes the KDL edit and runs the existing generate apply path
(write files, update lock). It inherits that path's drift and skew policy: a drifted
target file is refused (exit 3) unless the user re-runs with the appropriate flag, skew
routes through `upgrade`, and so on.

Exit codes reuse the CLI's `Code`:

- 0 Clean: written (or a no-op when the package is already present).
- 2 Usage: no resolvable host, unknown `--host`, more than one `default` host.
- 3 Drift: the target's generated file was hand-edited (from the apply path).
- 5 Validation: verification failed, or verification was skipped under `--strict`.

## Testing

- `package` module: unit tests that a node lowers to the `environment.systemPackages`
  list entry.
- list-merge: pipeline unit tests (two `package` nodes merge to one list, order
  preserved; a non-list duplicate path still conflicts).
- KDL splice: unit tests (append under an existing host, formatting and comments
  preserved, idempotent when already present).
- Host selection: unit tests for the resolution order, including `default = true` and the
  multiple-defaults error.
- Nix eval: an injectable checker so tests run without nix; a shim in integration tests.
  Real eval verified in the VM against the pinned rev.
- CLI end to end: `install` on a temp project drafts, previews, writes, and leaves
  `check` clean; `--strict` errors when the checker is absent.

## Files touched

- `crates/knixl-modules/src/builtin/package.rs` (new) and `builtin/mod.rs` (register).
- `crates/knixl-modules/src/builtin/host.rs`: accept the `default` prop in the schema.
- `crates/knixl-pipeline/src/lib.rs`: the list-merge step.
- `crates/knixl-cli/src/main.rs`: the `install` subcommand, drafting, preview, and the
  nix eval checker wiring.
- A new nix eval checker: likely `crates/knixl-nix` (alongside the formatter) or a small
  new module, injectable via an env shim.
- `docs/`: a short note on `install` and the `package` module.
- Tests across the above; a VM-verified end-to-end check.

## Out of scope (later slices)

The ratatui TUI (Slice C), fuzzy search and package suggestions, version pinning
(Slice D, may prove infeasible in plain nixpkgs), `nix build` of the derivation
(Slice B), and non-`systemPackages` targets.
