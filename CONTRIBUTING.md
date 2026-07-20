# Contributing to knixl

Read README.md, then docs/ in order, before writing code. `docs/adr/` records
decisions that should not be quietly reversed; if a change seems to need one
reversed, open an issue first rather than just doing it.

## Prerequisites

- **Rust**, via rustup. The toolchain is pinned in `rust-toolchain.toml`
  (1.87.0, with `rustfmt` and `clippy`); rustup picks this up automatically in
  the checkout.
- **Nix** on PATH, for the oracle (option-path validation against a pinned
  nixpkgs rev) and for the golden tests.
- **A formatter**: `nixfmt-rfc-style` (preferred) or `nixfmt` on PATH.
  `KNIXL_FORMATTER` overrides which binary is used, e.g. if it is not called
  `nixfmt` on your system.

## Workflow

The repo uses [mise](https://mise.jdx.dev/) to wrap the common commands. Plain
`cargo` works too, mise just saves typing:

```sh
mise run build   # cargo build
mise run test    # cargo test
mise run lint    # cargo clippy --all-targets -- -D warnings
mise run fmt     # cargo fmt --all
```

The tree is rustfmt-normalised: `cargo fmt --all` (or `mise run fmt`) must
leave it clean, and CI checks this with `cargo fmt --all --check`. Run it
before opening a PR.

## Golden tests are the acceptance tests

`examples/` holds worked hosts with their expected generated Nix under
`examples/expected/` and a matching `knixl.lock.kdl`. These are golden tests,
not decoration: the pipeline must reproduce them byte-for-byte after
formatting. They run under `cargo test -p knixl-pipeline --test golden` (also
covered by `mise run test`) and need a formatter on PATH (see Prerequisites);
without one, the formatter-dependent cases skip themselves rather than fail.

If a change alters generated output, update the relevant `examples/` fixtures
in the same PR and explain why in the description. An unexplained golden diff
will be treated as a regression, not a feature.

Determinism matters as much as correctness here: generation is deterministic
to the byte (no `HashMap` iteration in emit paths, a defined attr sort order,
stable list order from KDL source order), because the lockfile depends on it.

## Pull requests

- Keep PRs focused: one logical change per PR.
- `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, and
  `cargo test` (or the equivalent `mise run` tasks) should all pass before you
  open the PR.
- Explain the why in the PR description, not just the what: what problem this
  solves, and any alternatives you considered.
- If the change touches an ADR-documented decision (`docs/adr/`), say so
  explicitly and expect discussion before merge.

## Licence of contributions

knixl is dual licensed under Apache License, Version 2.0 (`LICENSE-APACHE`)
and the MIT licence (`LICENSE-MIT`). Unless you state otherwise, any
contribution you submit for inclusion is dual licensed as above, with no
additional terms.
