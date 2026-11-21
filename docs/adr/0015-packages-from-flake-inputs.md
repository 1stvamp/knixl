# ADR 0015: Packages from flake inputs

Status: accepted

Amends: ADR 0014 (flake inputs in the system flake) and ADR 0005 (package version pinning).

## Context

ADR 0014 lets a `system {}` declare flake inputs, but the only thing a host can take from an
input is a NixOS module, listed in `oracle-modules` with `input=`. It deferred passing
`specialArgs` or `inputs` to modules, and inputs that provide anything other than NixOS modules
(overlays, packages). A system migrating off a hand-written flake usually has at least one
package that comes from an input rather than from nixpkgs (a tool's own flake, or a fork), and
the usual way a hand-written flake reaches it is `specialArgs = { inherit inputs; }` plus a
module that reads `inputs.<name>.packages.${pkgs.system}.default`. Without either, that package
has no home in a generated system, so the migration stalls on it (issue #117).

## Decision

### 1. `inputs` as a module argument

In input mode, every `nixpkgs.lib.nixosSystem` call in `generated/flake.nix` passes
`specialArgs = { inherit inputs; }`: hosts, installers and guest images alike. It is emitted
after `system = ...;` when there is one, and before `modules`. Any module in the configuration
can then take `inputs` as an argument (`{ inputs, pkgs, ... }:`), which covers a module brought
in with a host `import` node. A module function listed in raw nix's own `imports = [ ... ];`
only works on a host with no `import` nodes and no side-files: raw-nix text is spliced into the
same attrset as the generated `imports` list, so otherwise `imports` is defined twice and nix
refuses it. The `import` node is the route to recommend.

The generated host module's own header stays `{ config, lib, pkgs, ... }:` unless the host
has a `package from=` (section 2). Listing `inputs` there unconditionally would break the
input-free flake (ADR 0009), which has no inputs to pass, and would change every golden for no
gain, since otherwise the knixl-generated body never refers to `inputs`.

The input-free flake (`eval-config.nix`) is unchanged byte for byte. It has no `inputs` to
pass, and so no `specialArgs`.

### 2. `package from=`

`package "handy" from="nixpkgs-unstable"` takes the package from a declared flake input
instead of the host's `pkgs`. It emits

```nix
(import inputs."nixpkgs-unstable" {
  config = config.nixpkgs.config;
  system = pkgs.stdenv.hostPlatform.system;
})."handy"
```

into `environment.systemPackages`, beside any ordinary `pkgs.<name>` entries. Passing
`config.nixpkgs.config` means the host's nixpkgs settings (e.g. `allowUnfree`) carry over to
the second nixpkgs, which is what a user expects when the same unfree package moves from one to
the other. The input has to be a nixpkgs checkout (or anything else whose `default.nix` takes
`{ config, system }`); an input that only exposes `packages.<system>` through its flake outputs
is reached through `specialArgs` and an `import` node or raw nix, as in section 1. Input and
attribute names are always quoted, the same as `inputs."disko".nixosModules."disko"` in the
flake. A dotted attribute name is split on `.` and each segment quoted, so
`package "kdePackages.kate" from="nixpkgs-unstable"` selects `."kdePackages"."kate"`, which is
what `pkgs.kdePackages.kate` resolves to on the plain path.

The pipeline adds `inputs` to the module's formals (`{ config, inputs, lib, pkgs, ... }:`) when,
and only when, the module's body reads `inputs."<name>"`, so a host without `from=` keeps the
header it had and no existing golden changes. The same applies to installer and guest-image
modules, which get `specialArgs` too. The check runs on the emitted body, so a `from=` package
inside a guest's `config {}` counts (the reference is lexically the host module's `inputs`).

Each package gets its own `import`. The let-hoisting pass only binds attrsets, lists and
indented strings, and making it bind applications would reach well beyond this feature, so two
packages from the same input bind the shared `{ config; system; }` argument once (as
`_knixl0`) and keep one `import` each. Nix caches the file import, but each application
evaluates its own nixpkgs instance, which is an evaluation cost only.

Refused, with the host file named in the error:

- `from=` together with `version=`. ADR 0005 pins a version by picking a nixpkgs commit, and
  `from=` has already picked the source; the two don't compose. Pin the input's `rev=` instead.
- `from="nixpkgs"`: that's the host's own `pkgs`, so the message says to drop `from=`.
- `from=` naming an input that isn't declared in `system {}`.
- Any `from=` outside input mode (no `input` nodes in `system {}`, or no `system {}` at all),
  since the module would read `inputs` and nothing passes it.

The first two are checked by the `package` module itself; the last two by the pipeline, which
is the only layer that sees `system {}`.

The oracle doesn't check package names for any package (a `package`-typed option accepts any
value), so a misspelt `from=` attribute name gets past `generate` the same way a misspelt
`pkgs.<name>` does, and fails at nix evaluation. `knixl install` only drafts plain `package`
nodes. It matches an existing node by name, so on a host that already has
`package "handy" from="nixpkgs-unstable"` it refuses `knixl install handy` (and `handy@1.0`)
with an error naming that node, rather than reporting the package as installed or rewriting
the node without its `from=`.

## Consequences

- Every input-mode flake gains one `specialArgs` line per configuration, so an existing
  input-mode project sees its `generated/flake.nix` go `Stale` on the next `generate`. The
  system it builds doesn't change unless a module starts reading `inputs`.
- `specialArgs` is fixed by the flake, so a module can't override `inputs` through
  `_module.args`, and `inputs` can be used in `imports` (which `_module.args` can't).
- A host that gains its first `package from=` sees its header change along with the package
  list; removing the last one takes `inputs` back out.
- Raw nix that reads `inputs` is outside what the oracle validates, the same as any other
  raw-nix text.
