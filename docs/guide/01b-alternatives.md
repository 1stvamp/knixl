# knixl and the alternatives

knixl compiles KDL into Nix source that you commit. Most tools nearby either go the other way, or bridge at evaluation time.

| Approach | What you write | What it produces | Where it differs from knixl |
| --- | --- | --- | --- |
| Hand-written NixOS modules | Nix | Nothing extra (Nix evaluates it) | Full power of the language. Mistakes surface at `nixos-rebuild`, and presets live in your own modules. |
| home-manager, niri-flake | Nix | KDL files | The mirror image: KDL is the output. |
| Nickel | Nickel | JSON, TOML, YAML | A configuration language that grew out of Nix and aims to fix some of its limitations. It doesn't export Nix source. |
| dhall-to-nix | Dhall | Nix expressions, usually at evaluation time | In nixpkgs it runs through import-from-derivation, so there's no readable committed source, and Dhall can't express the NixOS module system. |
| terranix | Nix | `config.tf.json` | The same generate, commit, apply shape, with a different source and target. |
| Nixtamal | KDL manifest | Pinned Nix inputs and a lockfile | A KDL manifest with a diff-friendly lockfile, for input pinning only. |
| knixl | KDL | NixOS modules, optionally `flake.nix`, and `knixl.lock.kdl` | Narrow on purpose: it writes option assignments and stops there. |

## What knixl learnt from them

dhall-to-nix's README lists what Dhall can't encode, the NixOS module system among them, so knixl doesn't try to be a general Nix-expression compiler: it only emits option assignments (plus `raw-nix` for the rest). terranix shows that an explicit generate step, with the output committed, is comfortable to live with. The home-manager pull request that added its KDL generator ([nix-community/home-manager#3399](https://github.com/nix-community/home-manager/pull/3399)) works through the mismatches between KDL and Nix data (bools and integers, arguments and properties, repeated nodes), which knixl meets from the other side.
