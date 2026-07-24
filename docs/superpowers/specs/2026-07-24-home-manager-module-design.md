# home-manager module (declarative, with guardrails)

Closes #12.

## Problem

home-manager (HM) configures a user's home environment. Used as a NixOS module it
is `home-manager.users.<name> = { ... }`, with a couple of integration settings at
`home-manager.*`. HM's own manual carries a "Words of warning" section: several
sharp edges (a mandatory `home.stateVersion`, the split-profile behaviour of the
NixOS-module integration, HM and NixOS fighting over `nix.gc`, session variables
that are not sourced in every context). knixl should make a safe subset of HM
easy to express and bake in the guardrails so a generated config avoids those
edges by construction.

This depends on the module-distribution story (#13, now shipped): the module
ships in the embedded stdlib, available in any project.

## Decisions (from brainstorming)

- **Declarative** module (`modules/home-manager/knixl-module.kdl`), shipped via the
  stdlib. `home.packages` is deferred: it needs package references (`pkgs.foo`)
  the template grammar cannot emit, the same limit tailscale/incus have.
- **Per-user node**, one per host in v1: `home-manager "<user>"`. The guardrail
  integration settings are global (`home-manager.useUserPackages` /
  `useGlobalPkgs`), so a second HM node on the same host would re-emit them and
  Nix would reject the duplicate attribute. Multi-user HM is a follow-up (it needs
  a built-in, or a grammar feature to dedup identical assignments, or nested
  repetition the single-level grammar lacks). v1 targets the common case: one
  primary user per host.
- **Safe subset**: enable HM for the user, require `home.stateVersion`, and expose
  session variables and `programs.<x>.enable`. Broad program configuration,
  `home.file`, `xdg.*`, and packages are follow-ups.

## KDL surface

`home-manager` claims the `home-manager` node.

```kdl
home-manager "wes" state-version="24.11" {
    session-var "EDITOR" "nvim"
    session-var "PAGER" "less"
    program "git"
    program "fish"
}
```

- `home-manager "<name>"` : the login name is the node label (a schema `arg`).
- `state-version="<rel>"` : required prop (see guardrails).
- `session-var "<name>" "<value>"` : repeated; two positional args (name, value).
- `program "<name>"` : repeated string child; enables `programs.<name>`.

## Emit

```
set "home-manager.useUserPackages" #true
set "home-manager.useGlobalPkgs" #true
set "home-manager.users.{name}.home.stateVersion" "{state-version}"
for-each "v" in "session-var" {
    set "home-manager.users.{name}.home.sessionVariables.{v.name}" "{v.value}"
}
for-each "p" in "program" {
    set "home-manager.users.{name}.programs.{p.name}.enable" #true
}
```

For the example above this lowers to (attr key order is the emitter's
BTreeMap-lexicographic order; the exact bytes are pinned by the golden):

```nix
home-manager.useGlobalPkgs = true;
home-manager.useUserPackages = true;
home-manager.users."wes" = {
  home = {
    sessionVariables = { EDITOR = "nvim"; PAGER = "less"; };
    stateVersion = "24.11";
  };
  programs = {
    fish = { enable = true; };
    git = { enable = true; };
  };
};
```

(`home-manager.users."wes"` may emit as a nested `users = { "wes" = {..}; }`
attrset; the golden captures the formatter's actual shape. `{name}` is a dynamic
attr key so it is `AttrKey::Quoted`; `{v.name}`/`{p.name}` likewise.)

## Guardrails (all four chosen)

- **`home.stateVersion` required.** A required prop, so a host cannot generate an
  HM config that HM would refuse to build for want of a stateVersion.
- **Safe NixOS-module integration.** `home-manager.useUserPackages = true` and
  `home-manager.useGlobalPkgs = true` are baked, so HM packages land in the system
  profile and HM uses the system nixpkgs, avoiding the split-profile and
  duplicate-nixpkgs surprises the manual warns about.
- **`nix.gc` / `nix.settings` left to NixOS.** The module never emits HM-level
  `nix.*`, so HM and NixOS do not fight over gc timers or nix configuration. This
  is enforced by omission and stated in the module docs.
- **Session-variable caveat surfaced.** `home.sessionVariables` is supported, and
  the caveat (they are sourced only in HM-managed shells, not every context such
  as display managers or non-login shells) is documented in the module summary and
  `docs/04`. A declarative module has no runtime-notice mechanism, so this guardrail
  is prose, not an emitted lint. That is a deliberate, stated limit, not an
  oversight.

## Acceptance tests

Golden host (mirroring nas/gateway/vmhost):

- `examples/hosts/workstation.kdl`: a host declaring `system` and a `home-manager`
  node with a stateVersion, two session variables, and two programs.
- `examples/expected/workstation.nix`: byte-exact nixfmt output, blessed (not
  hand-written).
- `crates/knixl-pipeline/tests/golden.rs`:
  - a structural test (identity formatter, unconditional) asserting the needles:
    `home-manager.useUserPackages = true`, `home-manager.useGlobalPkgs = true`,
    `home-manager.users."wes"`, `stateVersion = "24.11"`, `sessionVariables`,
    `EDITOR = "nvim"`, `programs`, `git`, `enable = true`;
  - a byte-exact `workstation_matches_golden` gated on `formatter_available()`;
  - a module-attribution assertion that the file lists `home-manager` (and `host`).

The module exercises only `set`, `for-each`, and dynamic attr-key interpolation,
all already covered by the grammar's own tests, so the golden is the module's
contract; no new `template.rs` unit tests are needed.

## Non-goals

- Multiple HM users per host (v1 is one node per host; the global guardrail
  settings collide otherwise). A follow-up.
- `home.packages` (needs a package-reference grammar feature; a follow-up shared
  with tailscale/incus).
- `home.file`, `xdg.*`, and rich per-program configuration: only
  `programs.<x>.enable` in v1.
- Emitting or managing `nix.gc` / `nix.settings` (deliberately left to NixOS).
- Standalone home-manager (not the NixOS-module integration); knixl generates
  NixOS modules, so only the `home-manager.users.<name>` integration is in scope.
- Importing the home-manager NixOS module itself: that is a system-assembly concern
  (the #40 flake or the hand-written seam), and validating `home-manager.*` needs
  the project to declare home-manager as an oracle module (#35), exactly like
  disko/sops. The module is oracle-agnostic.

## Determinism

Only `set` and `for-each` over KDL source order, all already deterministic. No
`HashMap`. Output is a pure function of the input.

## Docs

- `docs/04-template-grammar.md`: add `home-manager` to the declarative-modules
  section, with the node shape, the baked guardrails, the one-node-per-host v1
  limit, and the session-variable caveat.
- No new ADR: a feature within the settled module and emit model; it relies on the
  distribution decision already recorded in ADR 0010.
