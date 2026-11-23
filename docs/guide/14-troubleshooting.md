# Troubleshooting

The messages you're most likely to meet, what they mean and what to do. Each one is printed as `validation: ...`, `drift: ...` and so on, and the command exits with the code shown.

## Your KDL

**``hosts/laptop.kdl:7: no module claims node `opensh` ``** (exit 5)
No module handles a node with that name. Usually a typo; `knixl doc <node>` or **Browse modules** in the TUI shows the right names. If it's your own module, check its `claims-node` and that its file is at `modules/<name>/knixl-module.kdl`.

**``hosts/laptop.kdl:8: unknown child `prot` ``** (exit 5)
The node exists but doesn't take a child by that name. `knixl doc <node>` lists what it does take.

**``hosts/laptop.kdl:8: `port` has the wrong type``** (exit 5)
A value of the wrong kind, e.g. `port "22"` where the module wants the number `22`, or a bool written as `"true"` instead of `#true`.

**``hosts/laptop.kdl: `services.nopenope.enable` is not a NixOS option in this host's nixpkgs``** (exit 5)
The oracle found an option path that doesn't exist in this host's nixpkgs. If it's in a module you wrote, fix the path; if the option was renamed in a newer NixOS, the module needs updating for that release. See [Validation and the oracle](11-validation.md).

## Generated files

**`drift: generated/hosts/laptop.nix was hand-edited; refusing to overwrite (use --accept-drift)`** (exit 3)
Someone edited a generated file. Look at the edit with `git diff`, move anything worth keeping into the KDL or an override module, then `knixl generate --accept-drift`. [Day to day](05-day-to-day.md#when-a-file-has-drifted) has the details.

**`orphan: generated/hosts/nas.nix is no longer generated (use --prune to delete)`** (exit 6)
A host was removed or renamed and its old generated file is still there. `knixl generate --prune` deletes it.

**`version skew: recorded versions differ from the running tool; run knixl upgrade`** (exit 4)
Something recorded in the lockfile has changed version since it was written, and the output would change: knixl itself, a module, or the formatter. A different `nixfmt` is the usual culprit between teammates or in CI, because formatter releases format code differently; the lockfile records the version it expects (the `formatter` line). Run `knixl upgrade` to see the difference and `knixl upgrade --yes` to accept it, or, if you didn't mean to change versions, use the knixl and nixfmt versions the lockfile records.

## Releases, pins and inputs

**`host "laptop": nixpkgs release "25.11" is not resolved: run knixl upgrade`** (exit 5)
The host names a release that hasn't been turned into a commit yet. `knixl upgrade --yes`.

**`system {} requires each host to declare a resolved nixpkgs baseline`** (exit 5)
With a `system` block, every host needs a nixpkgs to build from. Add `nixpkgs release="25.11"` (or whichever release) to each host, then `knixl upgrade --yes`.

**`system {} with flake inputs needs every host on one nixpkgs baseline rev`** (exit 5)
In input mode every host builds from the one nixpkgs input, so they all need the same release. Either put them on the same release, or drop the `input` lines to use the input-free flake (see [More hosts](04-running-machine.md#more-hosts)).

**``input `nixpkgs`: url "github:NixOS/nixpkgs/nixos-25.11" carries a ref; pin it with rev= instead``** (exit 5)
Flake inputs are pinned by commit, never by branch. Drop the branch from the URL; for nixpkgs the commit comes from your hosts' release, and for other inputs add `rev="<commit>"` or let `upgrade` resolve one.

**``generated/flake.lock is missing: run `nix flake lock` in generated/ and commit it``** (exit 5)
In input mode, `check` compares Nix's lock with knixl's. After the first `generate`, and after any `upgrade` that moves an input: `git add generated`, then `(cd generated && nix flake lock)`, and commit.

**`module source "x": not resolved (no lock pin): run knixl install or upgrade`** (exit 5)
A module listed in `knixl.kdl`'s `modules` block hasn't been fetched yet. `knixl upgrade --yes` fetches and pins it.

## Nothing seems to be validated

If a deliberately wrong option path doesn't fail, look for a warning saying the host has no NixOS option set: `knixl upgrade --yes` builds it. A host with no `nixpkgs release` has no commit to validate against at all, so give it one. [Validation and the oracle](11-validation.md#turning-it-on) has the details.

## The TUI won't start

**`knixl: tui needs an interactive terminal`** (exit 2)
`knixl tui` needs a terminal on both its input and its output, so it refuses when either is piped or redirected. `knixl install` falls back to a plain prompt in the same situation, or none with `--yes`.
