# ADR 0003: Validate output against real NixOS options, not hand-written schemas

Status: accepted

Refined by: ADR 0007 (2026-07-20): the single global oracle rev this ADR pins becomes one
rev per host, falling back to the global rev for hosts that do not declare a release.

## Context

Emitted option paths need validating so a wrong or renamed option fails early. The obvious approach (hand-write an output schema per module) duplicates what NixOS already publishes and forces a recompile per module.

## Decision

Extract the NixOS option set via `nixosOptionsDoc` (`options.json`) and use it as the type oracle. Modules become curated presets on top of the full, already-typed option namespace. The nixpkgs rev is pinned in the lock.

## Consequences

- The arbitrary-option escape hatch is validated for free, and option docs come along.
- The oracle is best-effort: `options.json` gives type *descriptions* as strings, so it catches unknown paths and gross type mismatches, and punts on submodule interiors. That is most of the value; do not over-invest past it.
- The oracle rev is part of the generation boundary (a check passing under one rev and failing under another would break reproducibility), so it is in the lock alongside the formatter.
- Value conflicts between modules are not type errors and need a separate plan-time lint (ADR implied, see docs/02).
