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

Once a release is resolved, knixl keeps the host on that commit, so a plain `upgrade` never moves it. To take the security fixes and updates that land on a release (the job `nix flake update` does in a hand-written flake), ask for it:

```sh
knixl upgrade --refresh          # show which hosts would move, and to what
knixl upgrade --refresh --yes    # move them
```

`--refresh` re-resolves every `nixpkgs release` (each host's, and the project's) to the newest commit on its branch. A host pinned to an exact `rev=` stays where it is. Then `knixl generate`, in input mode `(cd generated && nix flake lock)` again, and rebuild. `upgrade` builds the option set for the new commit as part of the same run.

### Taking only some of it

A nixpkgs update moves everything at once: a nixpkgs commit is one consistent set of packages, and the binary cache only has builds of those sets. So knixl doesn't pick packages out of an update; it lets you decide whether to take the update, and how far.

- **Only when it fixes something.** `knixl upgrade --refresh --if-security` runs the [update report](#being-told-about-updates) first and only refreshes if the update fixes at least one CVE. Add `--min-severity high` (or `medium`, `critical`) to ignore lesser fixes. It needs vulnix, and refuses rather than guessing without it.
- **Only as far as a version.** `knixl upgrade --refresh --keep curl=8.19.* --keep linux=6.12.*` moves to the newest nixpkgs that still has each named package within its pattern (`*` matches anything). It only considers official channel releases (the commits Hydra tested and the cache built), and finds the right one by bisection, evaluating a handful of them, so it takes a minute or so. The package name is a nixpkgs attribute (`linux` is the kernel package). If even the next release breaks a constraint, nothing moves.
- **One application at a time.** For a package the host installs itself with `package`, the update report lists a `knixl install --host <host> <package>@<version>` line for each that changes: a [version pin](08-packages.md#pinning-a-version) that takes that one package's new version and leaves the rest of nixpkgs where it is. This only works for applications; a library like openssl is used by everything built against it, so its fixes come with the whole update.

The TUI's **Check for updates** screen puts these together: it runs the report, shows how many CVEs the update fixes, and lets you choose the whole update (optionally only if it fixes CVEs) or individual application pins, then applies them.

## Being told about updates

`knixl updates` reports what `upgrade --refresh` would change, without changing anything: for each host that would move, the package versions that change (classed as major, minor, patch, other or downgrade) and the security fixes among them.

```sh
knixl updates            # markdown, e.g. for a pull request
knixl updates --json     # for scripts
```

It refreshes a copy of the project, evaluates each host's system before and after (it doesn't build them), and diffs the two. Security fixes come from [vulnix](https://github.com/nix-community/vulnix), which it runs on both: a CVE flagged on the current system and not on the refreshed one is fixed, and the report lists each with its CVSS score and severity. A package whose version stays the same but loses a CVE shows as "patched". vulnix matches CVEs to packages by name across the whole build closure, so the list of what's still flagged afterwards (folded away at the end of the report) includes false positives; the fixes, being a difference between two scans, are the useful part. Without vulnix on the `PATH` (or `KNIXL_VULNIX` pointing at it), the report leaves security out and says so.

It needs a generated flake (a `system` block) and Nix, and the first run downloads vulnix's CVE database, so expect a few minutes.

### A daily pull request

This GitHub Actions workflow runs every morning, and when there's anything to take, opens (or updates) a draft pull request with the refresh applied and the report as its description, labelled `security` when the update fixes any CVEs:

```yaml
name: nixpkgs updates
on:
  schedule:
    - cron: "0 6 * * *"
  workflow_dispatch:
permissions:
  contents: write
  pull-requests: write
jobs:
  updates:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: cachix/install-nix-action@v31
      - uses: actions/cache@v4
        with:
          path: ~/.cache/vulnix
          key: vulnix-${{ github.run_id }}
          restore-keys: vulnix-
      - run: nix profile install nixpkgs#vulnix nixpkgs#nixfmt github:1stvamp/knixl
      - run: knixl updates > "$RUNNER_TEMP/report.md"
      - id: security
        run: |
          if grep -qE '^\*\*[1-9][0-9]* CVEs? fixed' "$RUNNER_TEMP/report.md"; then
            echo "labels=security" >> "$GITHUB_OUTPUT"
          fi
      - run: |
          knixl upgrade --refresh --yes
          knixl generate
          git add generated
          (cd generated && nix flake lock)
      - uses: peter-evans/create-pull-request@v7
        with:
          branch: nixpkgs-updates
          title: nixpkgs updates
          body-path: ${{ runner.temp }}/report.md
          draft: true
          labels: ${{ steps.security.outputs.labels }}
```

When nothing moves, the refresh changes no files and no pull request is opened. Merging the pull request is the update.

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
