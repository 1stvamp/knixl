# Day to day

Once a project is set up, most work is a short loop: edit the KDL, look at what changes, generate, commit. This page covers that loop, what each file state means, and what to do when knixl refuses.

## The loop

```sh
$EDITOR hosts/laptop.kdl
knixl plan        # what would change? writes nothing
knixl generate    # write it
git diff          # read the Nix it produced
git commit
```

`plan` and `check` both compute the expected output and compare it with what's on disk; neither writes anything. `plan` is for you, so by default it exits 0 whatever it finds (add `--detailed-exitcode` if you'd like it to exit with the same codes as `check`). `check` is for CI and exits non-zero unless everything is clean.

Read the generated diff before committing: it's the actual change to your machine.

The loop is the same when the change comes from `knixl install`, the TUI, or Browse's insert: they edit the KDL for you (and `install` regenerates too), and you review and commit as usual.

## What the states mean

Every generated file is in one of five states, which `plan` and `check` print. (`generate` only prints when something needs your attention.)

| State | What happened | What to do |
| --- | --- | --- |
| `clean` | The file matches what the KDL produces. | Nothing. |
| `stale` | You changed the KDL (or a module changed), so the file is out of date. | `knixl generate`. |
| `missing` | The file should exist and doesn't, e.g. a new host. | `knixl generate`. |
| `drifted` | Someone edited the generated file by hand. | See [drift](#when-a-file-has-drifted) below. |
| `orphaned` | A file knixl generated is no longer claimed by any host, e.g. you deleted a host. | `knixl generate --prune` deletes it. |

## When a file has drifted

A drifted file is one somebody edited by hand, and `generate` won't overwrite it: you'd lose their change without knowing it existed. It exits with code 3 instead. Look at what changed (`git diff generated/`), then pick one:

- **The edit should be kept.** Express it properly and the generated file can go back to being generated: put it in the KDL if a module supports it, or in a hand-written module that overrides the generated value (see [When knixl doesn't cover it](09-escape-hatches.md)). Then `knixl generate --accept-drift` regenerates the file, discarding the hand edit you've now moved.
- **The edit was a mistake.** `knixl generate --accept-drift` throws it away and regenerates.

`--accept-drift` is the only way past a drifted file, and it always means "regenerate this file over whatever is there".

## Upgrading knixl or a module

When you install a newer knixl, the built-in modules may produce slightly different Nix (a new default, a tidier layout). knixl won't let that change slip into a routine `generate`: if a version recorded in the lockfile has moved and the output would change, `generate` refuses with exit code 4 and points you at `upgrade`.

```sh
knixl upgrade        # show migration notes and what would change
knixl upgrade --yes  # apply it and record the new versions
```

`upgrade` is also where everything that needs the network happens: resolving a host's `nixpkgs release` to a commit, pinning flake inputs, and fetching third-party modules. Nothing is written or fetched until you pass `--yes`.

To move a host to a newer nixpkgs release, change its release (`nixpkgs release="26.05"`) and run `upgrade`.

## Taking updates within a release

Once a release is resolved, knixl keeps the host on that commit, and `upgrade` doesn't move it forward along the same release by itself. To take the security fixes and updates that land on a release (the job `nix flake update` does in a hand-written flake), find the current tip of the release branch and name it:

```sh
git ls-remote https://github.com/NixOS/nixpkgs refs/heads/nixos-25.11
```

```kdl
nixpkgs release="25.11" rev="<the commit it printed>"
```

Then `knixl upgrade --yes`, `knixl generate`, and in input mode `(cd generated && nix flake lock)` again, and rebuild. In input mode every host shares one nixpkgs, so update the `rev` on all of them together.

## In CI

Put this in your pipeline:

```sh
knixl check
```

It needs the formatter on the `PATH` (the same version you generate with, since the lockfile records it), and it never writes or prompts. It fails if anyone changed the KDL without regenerating, edited a generated file, or bumped knixl without running `upgrade`. In input mode it also fails if `generated/flake.lock` is missing or disagrees with the lockfile. Add `--json` for machine-readable output: one object listing each file's path and state, plus any warnings.

## What to commit

Everything except build results:

- `hosts/` and `knixl.kdl`: your source.
- `modules/`, if you've written any modules of your own.
- `generated/`, including `generated/flake.lock` if knixl writes a flake.
- `knixl.lock.kdl`.

## Exit codes

| Code | Meaning | Usual fix |
| --- | --- | --- |
| 0 | Clean, or applied cleanly. | |
| 1 | Internal error. | Please report it. |
| 2 | Bad command-line usage. | Check `knixl <command> --help`. |
| 3 | A generated file was edited by hand. | [Deal with the drift](#when-a-file-has-drifted). |
| 4 | A version change would alter the output. | `knixl upgrade`. |
| 5 | Validation failed: bad KDL, an unknown node or option, an unresolved release or pin. | Read the message; [Troubleshooting](14-troubleshooting.md) covers the common ones. |
| 6 | Regeneration owed: something is stale, missing or orphaned. | `knixl generate`. |

When several apply, the most serious wins, in this order: 5, 3, 4, 6.
