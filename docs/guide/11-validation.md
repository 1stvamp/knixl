# Validation and the oracle

knixl checks your work at two levels before it writes anything:

- **Your KDL** is checked against each module's schema. An unknown node, an unknown child, a missing required field or a value of the wrong type stops generation with exit code 5, naming the file and line. This always happens.
- **The Nix it would write** is checked against the real NixOS option set for the host's nixpkgs: every option path has to exist, and values have to be roughly the right type. knixl calls this option set the *oracle*. This only happens when the option set is available, and getting it there takes one step of setup.

## What the oracle catches

The most common mistake in generated Nix is an option path that's wrong: misspelt, renamed between NixOS releases, or set on the wrong release. The oracle catches all of those, at generate time:

```
validation: hosts/laptop.kdl: `services.nopenope.enable` is not a NixOS option in this host's nixpkgs
```

It also catches gross type errors (a string where NixOS wants a boolean) and options that are read-only.

It's deliberately best-effort beyond that. NixOS describes option types as text ("list of string", "attribute set of submodule"), and knixl understands the common ones and lets the rest through. It doesn't look inside free-form settings like `nix.settings`, and it can't see conflicts between two modules setting the same option (knixl warns about those separately).

## Turning it on

The oracle needs the NixOS option set for the nixpkgs commit a host is on, as an `options.json` file in knixl's cache. `knixl upgrade` builds it with Nix: when a host's `nixpkgs release` is first resolved, and on any later `upgrade` that finds one missing. The build takes a minute or so the first time; after that the file is reused by every project on the same commit (it lives in `~/.cache/knixl`, or `$XDG_CACHE_HOME/knixl`).

So the only thing to do is give each host a `nixpkgs release` and run `knixl upgrade --yes`. Until a host has an option set, `plan`, `generate` and `check` warn that it isn't validated:

```
warning: host "laptop": no NixOS option set for nixpkgs b6018f8..., so its options are not validated: run knixl upgrade --yes to build it
```

If the build fails (no network, say), `upgrade` carries on and warns. In CI, either cache `~/.cache/knixl` between runs (keyed on `knixl.lock.kdl`) and run `knixl upgrade --yes` when it's missing, or set `KNIXL_OPTIONS_JSON` to the path of a prebuilt `options.json`, which is then used for every host.

## Per-host option sets

Each host is validated against its own nixpkgs: the commit its `nixpkgs release` resolved to. A fleet that's part-way through moving from one release to the next validates every host against the options it will actually be built with. A host with no option set in the cache is generated without option checks, on its own; the other hosts are still checked. A host with no `nixpkgs release` of its own is checked against the project's, if `knixl.kdl` declares one (`nixpkgs release="25.11"` at the top level, resolved by `knixl upgrade` like a host's); with neither, it's only checked when `KNIXL_OPTIONS_JSON` is set.

## Modules from other flakes

Some options don't come from nixpkgs at all: `disko.devices.*` comes from disko, `sops.*` from sops-nix, `home-manager.users.*` from home-manager. To validate against those, declare the modules in `knixl.kdl`:

```kdl
oracle-modules {
    module "disko" flake="github:nix-community/disko"
    module "sops-nix" flake="github:Mic92/sops-nix" attr="default"
}
```

`attr` names the flake's `nixosModules.<attr>` and defaults to `"default"`. In a project with flake inputs, refer to the input instead (`module "disko" input="disko" attr="disko"`), and the same declaration also adds the module to every host's system.

`knixl upgrade` resolves each flake to a commit, builds the combined option set (nixpkgs plus these modules) with Nix and caches it, so this case needs no manual step. A host can replace the project's list with its own `oracle-modules` block, as long as it declares its own `nixpkgs release`.

## What isn't checked

To keep expectations straight, knixl validates the option paths its modules write. It doesn't validate:

- `raw-nix` text, or files brought in with `import`;
- the inside of free-form or submodule options it can't parse;
- secret references (it checks the option that holds the secret's path, and leaves the secret itself to sops-nix or agenix);
- whether the system builds. That's Nix's job: run `nixos-rebuild build` (or `nix build` on the generated flake) in CI as well as `knixl check` if you want both.
