# ADR 0009: System assembly flake emission

Status: accepted

Relates to: ADR 0001 (KDL is authoritative), ADR 0002 (emit source, not values), ADR 0005
(package version pinning), ADR 0007 (per-host baseline nixpkgs rev).

## Context

knixl emits one NixOS module per host (`generated/hosts/<h>.nix`), not a buildable system.
Turning that module into a `nixosConfigurations.<h>` that `nixos-rebuild` or `nixos-anywhere`
can consume, pinned to a nixpkgs, has been a hand-written flake sitting outside knixl. So the
reproducibility invariant `output = f(kdl, ...)` stopped at the module boundary: the assembly
flake was not part of `f`, and "the whole box" was not reproducible from knixl. The homelab
expressiveness review named this the gap between "generated modules" and "a box I can install".

The per-host baseline rev already exists (ADR 0007), so nixpkgs is pinnable per host, and a full
40-char git rev is a complete pure pin on its own (ADR 0005), so a flake can pin nixpkgs without
a `flake.lock`.

## Decision

knixl emits the system-assembly flake as an opt-in, generated and locked artefact. When the
project's `knixl.kdl` declares a `system {}` block, `knixl generate` writes
`generated/flake.nix` defining `nixosConfigurations.<host>` for every host; when it does not,
knixl emits modules only, exactly as before, and the assembly flake stays a deliberate
hand-written seam.

- **Opt-in via `knixl.kdl`**: a `system { state-version "<rel>" }` block. Its presence enables
  emission; its absence keeps today's modules-only behaviour. `state-version` supplies
  `system.stateVersion` (there is no other KDL source for it).
- **Generated and locked**: `generated/flake.nix` is part of `output = f(kdl, tool_version,
  module_versions, formatter_version, oracle_rev, baselines)`. It is formatted by the pinned
  formatter, hashed in `knixl.lock.kdl`, and reconciled `Stale`/`Drifted`/`Orphaned` like every
  other generated file (`--accept-drift` retakes the hash, `--prune` removes it when the block
  is dropped). A hand-edit is drift, not a silent overwrite.
- **Pure and input-free**: `knixl.lock.kdl` remains the single lock; there is no `flake.lock`.
  For each host, nixpkgs is pinned to that host's baseline rev via `builtins.fetchGit { url;
  rev; }` (a full rev is a pure pin, ADR 0005), and `nixosConfigurations.<host> = (that
  nixpkgs).lib.nixosSystem { modules = [ ./hosts/<host>.nix ]; }`. The system architecture flows
  through the module's `nixpkgs.hostPlatform`, already emitted from the host's `system` field.
- **The flake composes, it does not model deployment topology**: it imports each host's
  generated module set. Hardware, disko (issue #37), and secrets (issue #38) are generated
  modules a host produces, and the flake picks them up with no bespoke external-file import. A
  machine-specific file a host genuinely needs is imported through the existing `raw-nix`
  escape hatch inside the host module.
- **A resolved baseline is a precondition**: with `system {}` on, a host whose baseline rev is
  not resolved in the lock cannot be pinned, so `generate` refuses (exit 5) with a message
  pointing at `install`/`upgrade`, mirroring an unresolved package pin.

## Consequences

- The reproducibility boundary reaches the whole system: with `system {}`, `knixl generate`
  produces a directly bootable `nixosConfigurations` set, and `output = f(kdl, ...)` holds end
  to end rather than stopping at the module.
- The hand-written seam remains the deliberate default when `system {}` is absent, and the docs
  say so, so nobody expects a modules-only project's `generate` to be bootable.
- Determinism widens to `generated/flake.nix`: byte-stable emission, formatted by the pinned
  formatter, hashed and drift-tracked. The per-host baseline revs are what make the flake
  reproducible, so the flake and the lock move together, and a baseline change (an `upgrade`)
  is a flake change.
- The flake pins nixpkgs by `fetchGit` rev rather than a flake input, so it is self-contained
  and needs no `nix flake lock`, at the cost of not participating in the flake-input ecosystem
  (a deliberate trade: knixl owns the lock, not Nix).
- Deferred: disko (#37) and secrets (#38) as their own modules; a hardware-profile-from-KDL
  story; multi-architecture or non-NixOS targets; and anything beyond `nixosConfigurations`
  (home-manager configurations, deploy orchestration).
