# ADR 0002: Emit Nix source text, not Nix values

Status: accepted

## Context

dhall-to-nix produces Nix at evaluation time via import-from-derivation, yielding normalised values, not source you would read or commit. It also cannot express core Nixpkgs idioms.

## Decision

knixl emits Nix *source text*: readable, commented, committable. It builds its own IR and a deterministic pretty-printer, then pipes the output through a pinned formatter (`nixfmt-rfc-style`) for canonical layout.

## Consequences

- The emitter does not need to be pretty, only structurally correct and stable, because the pinned formatter owns final layout and only the post-format text is hashed and committed.
- Formatting policy lives in one external, version-locked place rather than smeared through the printer.
- The formatter version is load-bearing for reproducibility, so it is pinned in the lock. Bumping it is a reviewed change, not a silent one.
- knixl models a constrained subset of Nix (module bodies), not the whole language. See ADR 0003 and docs/01.
