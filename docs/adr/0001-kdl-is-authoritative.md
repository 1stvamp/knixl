# ADR 0001: KDL is authoritative, generated Nix is derived

Status: accepted

## Context

A KDL-to-Nix tool could try to be bidirectional: parse hand-edited Nix back into KDL and keep the two in sync. Users will ask for it.

## Decision

KDL is the single source of truth. Generated Nix is a derived, disposable build artefact. There is no round-trip from edited Nix back to KDL. Overrides live in separate, hand-written Nix modules that import the generated ones and use the module system (`lib.mkForce`, `lib.mkAfter`).

## Consequences

- Drift detection stays whole-file: a generated file that differs from its lock hash was hand-edited, full stop. No AST diffing, no marker parsing.
- "Override anything within reason" is free via the module system, because the output is the NixOS module system. Anything expressed as an option is overridable from a sibling.
- What is not overridable without going up to KDL: knixl's structural choices (which files, which modules). That is the correct boundary and is stated in user docs.
- Bidirectional sync is where codegen tools go to die (dhall-nix and every round-tripping tool learned this). We do not go there.
