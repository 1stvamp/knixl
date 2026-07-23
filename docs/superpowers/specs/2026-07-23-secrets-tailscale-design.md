# Secrets model (reference-by-name) plus a tailscale module

Closes #38.

## Problem

knixl has no way to reference a secret, which blocks anything carrying a
credential. The immediate case is the Tailscale auth key, which on NixOS becomes
`services.tailscale.authKeyFile`. Secret material is outside the byte-reproducible
KDL-to-Nix boundary by nature: the encrypted blob lives out of band (in the
user's sops-nix or agenix config), and knixl must never see or hash the plaintext.
So the model is reference-only: knixl emits the wiring from a module option to the
decrypted path, and that emitted wiring is itself the record that a secret is used.

## Design decisions

- A first-class **grammar value form** `(secret)"name"`, not a one-off built-in.
  #38 is a reusable secrets model, so any declarative module can wire a secret,
  and tailscale then lands as a declarative module (like zfs/user/openssh) rather
  than Rust. Additive grammar change, in the same spirit as `(collect-opt)`.
- **Backend is configurable, project-level, default sops-nix.** `knixl.kdl` gains
  `secrets backend="sops-nix"` (or `"agenix"`). The two differ only in the option
  prefix, so supporting both is cheap.
- **Inline reference, no declaration node.** A module names the secret inline; there
  is no separate `secret "name"` declaration and no dangling-name check. knixl emits
  the reference; a typo surfaces at NixOS build time, not at knixl generate. This is
  the smallest surface consistent with "reference-only".

## Grammar: the `(secret)` value form

In `crates/knixl-modules/src/template.rs`.

- `enum ValueTemplate`: add `Secret(Vec<StrPart>)`. The argument is an interpolated
  string naming the secret, so a module can write a literal name or interpolate a
  binding: `(secret)"tailscale-authkey"` or `(secret)"{k.secret}"`.
- `parse_value`: map the `(secret)` type annotation to `Secret(parse_str_parts(s))`,
  alongside the existing `(collect)` / `(collect-opt)` / `(indent-str)` arms.
- Interpret: resolve the parts to the secret name (reuse `interp_parts`), then emit a
  `NixExpr::Raw` holding `config.<prefix>."<name>".path`. It must be `Raw`, not
  `NixExpr::Select`: the emitter (`knixl-ir/src/emit.rs`) renders `Select` segments
  raw with a bare dot, so a hyphenated name like `tailscale-authkey` would emit
  invalid Nix (`config.sops.secrets.tailscale-authkey.path`, parsed as subtraction).
  `Raw` passes through verbatim, exactly as `backups` emits its runtime `when`
  condition. The name is double-quoted and escaped (backslash and double-quote) so it
  cannot break out of the string literal.
- Dry type-pass (`check_stmts`): handle `Secret` like `Str`, each `{lookup}` part must
  resolve to a scalar. So `(secret)"{k.secret}"` type-checks that `k.secret` is a scalar.

`config` is a lambda formal of every generated module (the header emits
`{ config, lib, pkgs, ... }`), so the reference resolves.

### Backend prefix

A `SecretsBackend` enum with two variants selects the prefix:

- `SopsNix` (default): `config.sops.secrets."<name>".path`
- `Agenix`: `config.age.secrets."<name>".path`

## Backend threading

The backend is project config that has to reach the emit path.

- `crates/knixl-pipeline/src/project.rs`: add `SecretsBackend { SopsNix, Agenix }`
  (default `SopsNix`) and `ProjectConfig.secrets_backend`. Parse a top-level
  `secrets backend="sops-nix"|"agenix"` node in `knixl.kdl`; an absent node or absent
  `backend=` yields the default. An unrecognised backend string is a parse error
  (`ProjectError`), not a silent fallback.
- `knixl-modules`: `LowerCtx` carries the backend. Since knixl-modules must not depend
  on knixl-pipeline, define the backend enum in knixl-modules (e.g. `SecretsBackend`)
  and have the pipeline map its `project::SecretsBackend` onto it, the same split
  already used for `ResolvedPin`/`PinStrategy`. `LowerCtx::new` gains the backend
  argument.
- `DeclarativeModule::lower` reads the backend from `ctx` and passes it into
  `EmitTemplate::interpret`, which threads it to the `Secret` arm. `interpret`,
  `run`, and `ValueTemplate::interpret` gain a backend parameter. Built-in modules
  ignore it.
- `crates/knixl-pipeline/src/lib.rs`: `generate` and `generate_one` gain a
  `secrets_backend` parameter, passed into `LowerCtx::new`. `gather` already parses the
  project config (from #40), so it passes `project.secrets_backend` through. Update the
  other `generate` call sites (the CLI, the golden-test harness) to pass the default.

## tailscale module

`modules/tailscale/knixl-module.kdl`, a declarative module.

```kdl
module name="tailscale" version="1.0.0" {
    summary "Tailscale with an auth key from a named secret and optional up-flags."
    claims-node "tailscale"

    schema {
        child "up-flag" type="string" repeated=#true \
            doc="A flag appended to services.tailscale.extraUpFlags, e.g. \"--ssh\"."
        child "auth-key" repeated=#true \
            doc="Wire services.tailscale.authKeyFile to a named secret. At most one." {
            prop "secret" type="string" required=#true
        }
    }

    emit {
        set "services.tailscale.enable" #true
        set "services.tailscale.extraUpFlags" (collect-opt)"up-flag"
        for-each "k" in "auth-key" {
            set "services.tailscale.authKeyFile" (secret)"{k.secret}"
        }
    }
}
```

Notes:

- `up-flag` is a repeated string child collected via the existing `(collect-opt)`, so
  an omitted one emits no `extraUpFlags` line (keeping the NixOS default) and the
  homelab's `--ssh` is just `up-flag "--ssh"`. The module exposes the general knob,
  not a baked `--ssh`.
- `auth-key` is a 0-or-1 structured child with a required `secret=` prop, wired through
  `for-each`, so a host that omits it emits no `authKeyFile` and Tailscale still works
  via interactive auth. More than one `auth-key` emits the option more than once (last
  wins under Nix), the user's error to make, same idiom as zfs `arc-max-bytes`.
- `services.tailscale.{enable,extraUpFlags,authKeyFile}` are stock in-tree NixOS
  options, so the oracle already covers them. Only the emitted value references
  `config.sops.secrets.*` / `config.age.secrets.*`, and the oracle validates option
  paths from assignment paths, not values, so no oracle-module dependency is added.

## Acceptance tests

Golden (in `crates/knixl-pipeline/tests/golden.rs`, mirroring `nas`):

- `examples/hosts/gateway.kdl`: a host declaring `system` and a tailscale node with an
  `auth-key secret="tailscale-authkey"` and `up-flag "--ssh"`.
- `examples/expected/gateway.nix`: byte-exact nixfmt output, blessed (not hand-written),
  under the default sops-nix backend, so `authKeyFile = config.sops.secrets."tailscale-authkey".path`.
- A structural test (identity formatter, unconditional) asserting the needles:
  `services.tailscale.enable = true`, `services.tailscale.extraUpFlags`, `"--ssh"`,
  `services.tailscale.authKeyFile = config.sops.secrets."tailscale-authkey".path`.
- A byte-exact `gateway_matches_golden` gated on `formatter_available()`.
- A module-attribution assertion that the file lists `tailscale`.

Unit tests in `crates/knixl-modules/src/template.rs`:

- `(secret)"literal-name"` under `SopsNix` emits `config.sops.secrets."literal-name".path`.
- the same under `Agenix` emits `config.age.secrets."literal-name".path`.
- `(secret)"{child.prop}"` interpolates the binding into the name.
- a secret name containing a double-quote is escaped in the emitted reference (cannot
  break out of the Nix string literal).
- dry type-pass: `(secret)"{x}"` where `x` resolves to a non-scalar fails at module load.

Project-config test in `crates/knixl-pipeline/src/project.rs`:

- `secrets backend="agenix"` parses to `SecretsBackend::Agenix`; absent node defaults to
  `SopsNix`; an unknown backend string errors.

## Non-goals

- No secret declaration node, no set of known secret names, no dangling-name validation.
- No generation of the backend's own secret declarations (`sops.secrets.<n> = {...}` or
  `age.secrets.<n> = {...}`); those live out of band in the user's config. knixl only
  emits the reference to `.path`.
- No plaintext handling, no encryption, no key management. knixl never reads the material.
- No per-secret backend override; the backend is one project-wide setting.
- No tailscale knobs beyond enable, extraUpFlags, and authKeyFile (no exit-node,
  advertise-routes, etc.); those are follow-ups.

## Determinism

The `(secret)` form is a pure function of (resolved name, backend): same input yields the
same `Raw` text. No `HashMap` on any emit path. The backend is a single project-wide value,
so output stays a pure function of the inputs, as the lock requires.

## Docs

- `docs/04-template-grammar.md`: document the `(secret)` value form under Values, and add
  tailscale to the module list.
- `docs/06-oracle.md`: a line that a secret reference emits a `config.<backend>.secrets.*`
  value (not an option path), so it is not oracle-validated, and that the backend is set by
  the project `secrets backend=` node.
- No new ADR: this is a feature within the settled module and emit model.
