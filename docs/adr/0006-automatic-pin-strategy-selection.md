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
  - **override**: `pkgs.<name>.overrideAttrs` with `version` and `src` inherited from the
    historical commit's package, built against the *baseline's* dependencies. No separate
    source hash is resolved or prefetched: `src` comes from the pinned commit. This is the
    lean result (one nixpkgs, integrated with the baseline), but old source against newer
    dependencies is what can genuinely fail to build.
  - **commit-mix** (ADR 0005): the whole package from the historical commit, built against
    its *own* era's dependencies. Robust (a self-contained historical closure that builds by
    construction), at the cost of pulling in a second nixpkgs.
- **Selection is automatic** at pin time (`install`/`upgrade`) and prefers the lean option
  that actually builds: resolve the commit, then build-test **override first**; if it builds,
  use it; otherwise fall back to commit-mix; if neither builds, refuse. Preferring override
  is deliberate: commit-mix almost always builds (its own deps), so trying it first would
  make it the perpetual winner and override dead code. The winning strategy is recorded in
  the lock. Nothing builds again at `generate`/`check`: those stay offline and pure.
- When selection cannot run (see skip conditions) **commit-mix is the safe default**: it is
  the option that builds without a feasibility test.
- The chosen strategy is stored in the lock pin. An absent strategy reads as commit-mix
  (back-compatible with ADR 0005 locks).
- **Flakes are not part of the picker.** A flake nixpkgs-input pinned to a commit produces
  the identical derivation as commit-mix, so it offers no different build outcome to test,
  and it pulls the project toward a flake shape it does not target (ADR 0001). It stays
  deferred.
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

- A pinned version is served by the lean `override` whenever its old source builds against
  the baseline, and by the robust commit-mix when it does not, both without user
  intervention, and the choice is reproducible (locked).
- knixl builds at pin time when a cross-rev pin is created and nix is present. The skip
  conditions keep the common cases (repeat installs, same-rev, nix-absent) build-free.
- Both strategies pull `src` from a commit that ships the version, so neither rescues "no
  commit ships this version at all": an unresolvable version still refuses (ADR 0005,
  unchanged).
- The reproducibility boundary gains the strategy field: the same inputs plus the recorded
  strategy reproduce the same emit byte-for-byte.
- `override` is build-tested before being locked precisely because old source against
  baseline dependencies can fail; when it does, commit-mix (which builds against its own
  era's dependencies) is the recorded fallback.
- The override feasibility test builds against the oracle baseline rev when one is recorded,
  falling back to the builder's channel otherwise (per-host baseline revs are #22), so the
  test and the emitted result can build against different nixpkgs until #22 lands.
