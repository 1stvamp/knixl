# ADR 0008: Out-of-tree oracle modules

Status: accepted

Refines: ADR 0003 (validate against real NixOS options) and ADR 0007 (per-host baseline
nixpkgs rev). Relates to: ADR 0005 (pinning).

## Context

The oracle (ADR 0003) validates every emitted option path against the NixOS option set for
one pinned nixpkgs rev. That set is exactly what `nixosOptionsDoc` produces from nixpkgs
itself, so a module that targets an option from an out-of-tree NixOS module, e.g. disko's
`disko.devices.*` or sops-nix's `sops.*`, has no home in it: the path is genuinely unknown to
nixpkgs alone, and the oracle refuses it as `UnknownOption` even though the generated Nix would
evaluate cleanly once the real system imports that module. A fleet that wants knixl to validate
disko or sops-nix output has to either turn the oracle off for those paths or accept
false positives.

ADR 0007 already lifted the oracle from one global rev to one rev per host. The same
per-host split is needed for module sets: a fleet migrating host by host onto disko does not
want every host augmented with it at once.

## Decision

The oracle's option set spans nixpkgs at its pinned rev plus every declared, pinned
out-of-tree module.

- **`knixl.kdl` declares the project's defaults**: an optional `nixpkgs release="<rel>"` (the
  project-wide baseline release, mirroring a host's own) and an optional `oracle-modules`
  block, each `module "<name>" flake="<ref>" [attr="<attr>"]` naming a flake to pull a NixOS
  module from (`attr` defaults to `"default"`).
- **A host may override the set, in full, but only alongside its own baseline**: a host's own
  `oracle-modules` block replaces the project's default for that host (no merging); declaring
  one requires the host to also declare its own `nixpkgs release="<rel>"` (ADR 0007), because
  the per-host `baseline` line is the only place in the lock able to carry per-host module
  pins. A host that declares `oracle-modules` with no declared release is refused with a clear
  error (exit 5): there is nowhere to store what would be resolved.
- **Resolution is at pin time only** (`install`/`upgrade`), mirroring ADR 0005/0007: each
  declared flake ref resolves to a `{url, rev}` pair (`git ls-remote` by default,
  `KNIXL_MODULE_RESOLVER` overrides), recorded as an `oracle-module` line under the project's
  `oracle` (the default set) or a host's own `baseline` (an override). `generate`/`check` stay
  offline; a change to the declared set is an `upgrade`-gated event, never a silent `generate`.
- **The augmented `options.json` is built, not hand-populated**: `install`/`upgrade` run
  `nixosOptionsDoc` over the pinned nixpkgs rev plus each resolved module (as a flake's
  `nixosModules.<attr>`), cache the result keyed by the effective set (the rev and the module
  pins together, order-sensitive), and record the built content's hash as an `options-hash`
  alongside the existing rev. Fetching the BASE (no-modules) set stays the manual step it
  already was (docs/06); only the augmented build is new. A missing nix is best-effort (a
  warning, unless `--strict`); a nix that runs but fails to build the declared set is always a
  hard error, since that means the declared module set itself is broken, not merely unverified.
- **Planning picks the effective set per host**: a host without its own override validates
  against the project's default module set; one with an override validates against its own.
  Either way the set is looked up from the lock's already-resolved pins, keyed by the same
  (rev, modules) pair the build used, so `generate`/`check` need neither nix nor the network.

## Consequences

- A declarative module can target an out-of-tree option once its flake is declared: the
  escape-hatch and curated-preset paths (ADR 0003) both extend to disko, sops-nix, and similar
  modules, validated the same way nixpkgs' own options are.
- The reproducibility boundary widens again: a module's `url`/`rev`/`attr` join the nixpkgs rev
  and options hash already there, per project default and per host override. Two projects
  pinned to the same rev and module set share the same cache entry; either project changing
  either one is a real, `upgrade`-gated change.
- The host-override-requires-a-release rule is a real modelling constraint, not a
  simplification to relax later without another lock-shape decision: there is no project-wide
  place to carry a per-host module pin.
- Still deferred: merging (rather than replacing) a host's module set against the project
  default, and validating the module flakes themselves (a module that fails to resolve or
  build is refused, but nothing here checks the module's own option set is sane before that).
