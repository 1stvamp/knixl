# ADR 0005: Per-host package version pinning via historical-commit mixing

Status: accepted

## Context

`knixl install <pkg>` installs whatever version of a package the locked nixpkgs
rev happens to ship. Users want a specific version on a specific host
(`install pkg@version`). Plain nixpkgs offers no clean per-package version
selection: a single commit ships one version of each package, and there is no
`pkgs.<name>."1.2.3"` attribute.

The options for pinning a version are:

1. **Historical-commit mixing.** Find a nixpkgs commit that ships the wanted
   version, import that commit alongside the host's baseline nixpkgs, and take
   just that package from it (`(import (fetchTarball <commit>) {}).<name>`). The
   version-to-commit map comes from an external index (nixhub.io, lazamar's
   nix-package-versions) since nixpkgs has none. Deterministic once the commit
   is resolved and locked.
2. **`overrideAttrs` version+src+hash.** Override the package's version and
   source in an overlay. Fragile: old versions rarely build against current
   build inputs, and the user must supply the source hash.
3. **Flake inputs per package.** A flake with multiple nixpkgs inputs. Same
   substance as (1), flake-shaped; knixl does not target flakes.

Two cross-cutting risks apply to any approach that pulls a package from a
different rev than the host baseline: **ABI mismatch** (an old package pulled
into a newer environment can fail to build or collide), and a **third-party
version index** dependency at pin time.

## Decision

Support `knixl install pkg@version` via **historical-commit mixing (option 1)**,
scoped per host:

- The **global baseline nixpkgs rev** (the oracle's) is unchanged and supplies
  every unpinned package. Per-host *baseline* revs are out of scope (deferred).
- A pinned package is recorded **per host in the lock** as
  `pin "<name>" version="..." nixpkgs-rev="<commit>" sha256="..."`. The KDL holds
  only the intent (`package "<name>" version="1.2.3"`); the resolved commit and
  hash are derived data and live in the lock, like the oracle rev and module
  versions (consistent with ADR 0001).
- Version-to-commit resolution is an **injected command** (`KNIXL_PIN_RESOLVER`,
  default queries nixhub.io and prefetches the sha256). It runs only at pin time
  (`install` / `upgrade`); the result is locked, so `generate` and `check` stay
  offline and pure.
- The generator emits a pinned package from a let-hoisted import of its locked
  commit, mixed into the host's baseline (Nix permits several nixpkgs revisions
  in one configuration). ABI breakage is caught by `install --build` (ADR-less
  slice B), not hidden.
- A KDL-declared version with no matching lock pin is a validation error that
  tells the user to run `install`/`upgrade` to resolve it, never a silent
  network fetch during generation.

## Consequences

- Users get real per-package, per-host version control, and one host can run
  several packages at different versions.
- knixl gains a soft dependency on an external version index at pin time only.
  Offline or without the resolver, a pin cannot be created (a clear error); it
  never resolves to a wrong commit silently.
- The reproducibility boundary widens: each pin's `(commit, sha256)` joins the
  lock as reproducibility data, and a pinned host fetches more than one nixpkgs.
- Cross-rev ABI mismatch is a real failure mode. Pairing pinning with `--build`
  turns it into an install-time refusal rather than a broken host.
- Deferred, and documented here so the decision is not silently reversed:
  per-host baseline revs, `overrideAttrs`/flake resolution paths, and garbage
  collection of unreferenced pins.
