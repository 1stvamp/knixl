# ADR 0006: Automatic pin strategy selection with ABI feasibility testing

Status: accepted

Amends: ADR 0005 (which deferred `overrideAttrs` as a resolution path).

## Context

ADR 0005 pins a package version by resolving it to a historical nixpkgs commit and
mixing that whole package into the host baseline (commit-mix). It named cross-rev ABI
mismatch as a real failure mode and deferred `overrideAttrs` and flakes. The #23 spike
(docs/superpowers/specs/2026-07-17-cross-rev-resolution-spike.md) then found that
`overrideAttrs` can be built with no extra source-hash resolution by pulling `version`
and `src` straight from the historical commit's package, which makes a second strategy
tractable without a new resolver. "Prepare for the worst" is the motivation: have a
working fallback ready before a real cross-rev break bites, and let knixl pick it
automatically rather than leaving a user with a broken host.

## Decision

Introduce a **pin strategy** recorded per pin, selected automatically at pin time by
build-testing the emitted expression against the host baseline.

- Two strategies, both derived from the same resolved commit:
  - **commit-mix** (ADR 0005, unchanged, the default and preferred): the whole package
    from the historical commit, built against its own era's dependencies.
  - **override**: `pkgs.<name>.overrideAttrs` with `version` and `src` inherited from the
    historical commit's package, built against the baseline's dependencies. No separate
    source hash is resolved or prefetched: `src` comes from the pinned commit.
- **Selection is automatic** at pin time (`install`/`upgrade`): resolve the commit, then
  build-test the strategies **as they would be emitted into the host** (catching eval and
  collision failures, not just isolated builds). commit-mix is tried first; `override` is
  the fallback, used only when commit-mix's build fails. The winning strategy is recorded
  in the lock. Neither builds again at `generate`/`check`: those stay offline and pure.
- The chosen strategy is stored in the lock pin. An absent strategy reads as commit-mix
  (back-compatible with ADR 0005 locks).
- **Flakes are not part of the picker.** A flake nixpkgs-input pinned to a commit produces
  the identical derivation as commit-mix, so it offers no different ABI outcome to test,
  and it pulls the project toward a flake shape it does not target (ADR 0001). It stays
  deferred.

### Skip conditions (no build)

The feasibility build is skipped, and commit-mix used, when:

- a lock pin for `(host, package, version)` already exists and the baseline rev is
  unchanged (idempotent: reuse the recorded strategy, no re-resolve, no re-build),
- the resolved commit equals the host baseline rev (no cross-rev, nothing to test), or
- nix is not available (cannot build-test; commit-mix with a warning, as with `--build`).

When `--build` is set, the feasibility build and the package build are the same build and
run once. A `--no-abi-check` opt-out skips selection and takes commit-mix.

## Consequences

- A pinned package that would break under commit-mix can be served by `override` without
  user intervention, and the choice is reproducible (locked).
- knixl builds at pin time when a cross-rev pin is created and nix is present. The skip
  conditions keep the common cases (repeat installs, same-rev, nix-absent) build-free.
- `override` only helps when a commit ships the version (its `src` comes from that commit);
  it does not rescue "no commit ships this version at all". Both strategies need such a
  commit, so an unresolvable version still refuses (ADR 0005, unchanged).
- The reproducibility boundary gains the strategy field: the same inputs plus the recorded
  strategy reproduce the same emit byte-for-byte.
- `overrideAttrs` builds old source against baseline dependencies, so it can still fail;
  that is why it is the fallback, not the default, and why it is build-tested before being
  locked.
