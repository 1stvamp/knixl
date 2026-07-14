# HANDOFF

State of play for the next session. Read this before trusting any `.rs` file in here.

## What this package is

The complete design for knixl, worked out in a chat session, packaged so a Claude Code session can continue. It is design plus specification plus a behaviour contract (the examples). It is not a working build.

## What exists and how much to trust it

- **Specs (docs/):** high confidence. These are the intended design and the ADRs are decisions, not suggestions. Follow them.
- **ADRs (docs/adr/):** decisions. Do not reverse without flagging.
- **Rust (crates/):** specification-grade sketches. The types, trait shapes, control flow, and command policy are the intent and should be preserved. But:
  - Bodies marked `/* ... */` are not written.
  - Helper functions used in examples (`assign(..)`, `child_arg_str(..)`, `child_flag(..)`, `unit_default(..)`, `bare()`, `interp_str(..)`, etc.) are referenced but not defined. Define them in the obvious crate.
  - Cross-crate imports are written as intent (`use knixl_ir::...`) but the crates are not yet wired end-to-end.
  - It will not `cargo build` as delivered. Task one is to make it compile with stubbed `lower()` bodies, then fill in.
- **Examples (examples/):** the behaviour contract. The `.kdl` inputs and the `expected/*.nix` outputs are what the pipeline must reproduce. Hashes in `examples/knixl.lock.kdl` are placeholders (`blake3:...`), to be recomputed once the emitter and formatter are real.

## What was deliberately deferred (open, not decided)

- **`EmitTemplate` type-check pass at module-load time.** The spec says a declarative module's template is dry-checked when loaded (so `{acme.email}` resolving to a non-scalar fails at load, not at generate). The mechanism is described in docs/04, not sketched in code.
- **`lib.mkIf` from a declarative module.** Runtime conditions off `config.*` are Rust-only for now (the `condition=` form). Declarative modules only get `when-flag` (generation-time). Revisit only if a real module needs it.
- **Cross-module value-conflict lint.** docs/03 specifies a plan-time lint: multiple assignments to the same `AttrPath` across modules in one file, warn unless priorities disambiguate. Not sketched. This closes the one breakage class the oracle structurally cannot catch (value conflict, not type error), so it is worth doing early.
- **`let` hoisting pass.** Deduplicating repeated sub-expressions into a `let` is named as a generator pass, not designed. Low priority.
- **Oracle depth.** `options.json` gives option types as human-readable strings, not structured types. The oracle does best-effort structural checking (catches unknown paths and gross type mismatches, punts on submodule interiors). Do not over-invest in parsing every type description; the path-existence check is most of the value.

## Known sharp edges

- `nixfmt-rfc-style` output changes across versions. If you bump it and the golden outputs move, that is expected: the version is in the lock precisely so this is a reviewed change, not a silent one.
- KDL v2 is the default in the `kdl` crate. Inputs are v2. Do not enable v1-fallback unless there is a real reason (it pulls in the whole v1 parser).
- The `raw-nix` escape is passed through verbatim but must still be validated as parseable Nix before emit, so a syntax error points at the KDL span, not at `nixos-rebuild`. The validator is specified, not written.

## Where the design came from

A single research-plus-design chat. The prior-art findings in docs/00 are from web search (home-manager PR 3399, Nickel, dhall-to-nix, terranix, Nixtamal, the `kdl` crate). Everything else is worked design. No code was run, so treat the Rust accordingly.
