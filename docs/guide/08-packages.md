# Packages and pins

Installing a package is the change you'll make most often, so knixl has a command for it that writes the KDL, checks the result and regenerates in one go.

## Adding a package

```sh
knixl install ripgrep
```

This adds `package "ripgrep"` to a host's KDL file (keeping the file's formatting and comments), generates the host with it, checks with Nix that `pkgs.ripgrep` exists, shows you the change and, once you confirm, regenerates. In a terminal it opens the [install screen of the TUI](06-the-tui.md#install-a-package); when piped it asks a plain yes/no question instead, and `--yes` skips the question.

Which host? `--host laptop` if you say so, otherwise the host marked `default=#true`, otherwise the only host if there's just one.

A few flags:

- `--build` also builds the package, to prove it builds on your nixpkgs as well as existing there. It's slower, so it's mostly worth it for something unusual.
- `--strict` makes a missing Nix an error. Without it, `install` skips the Nix check with a warning.

You can also just write `package "ripgrep"` in the host file yourself and run `knixl generate`; `install` is a convenience that also checks the package resolves.

## Pinning a version

Sometimes you need an exact version, older or newer than the one in your nixpkgs:

```sh
knixl install curl@8.4.0
```

knixl looks the version up in the nixhub version index, finds a nixpkgs commit that had `curl` at 8.4.0, and records it in the lockfile against the host. The KDL says what you asked for:

```kdl
package "curl" version="8.4.0"
```

and the lockfile says how it was resolved. The generated Nix then builds the package from that commit, which it fetches itself with `builtins.fetchGit` when the system is evaluated. That fetch sits outside your flake's inputs and `flake.lock`, so the first evaluation after adding a pin downloads that nixpkgs. There are two ways it can do that, and knixl picks one for you when you pin:

- **override**: builds the old version's source with your current nixpkgs' compilers and libraries. Smaller, and it shares dependencies with the rest of the system, but it only works if that source builds against today's dependencies.
- **commit-mix**: takes the whole package from the old nixpkgs commit, dependencies and all. Always works, but pulls in a second copy of everything it depends on.

knixl tries a build with override first and falls back to commit-mix if it fails. The choice is recorded in the lockfile (`strategy="override"` on the pin line; no strategy means commit-mix). `--no-abi-check` skips the trial build and always takes commit-mix, which is quicker when you know you want it. Pair a pin with `--build` to be sure the pinned package really builds.

A version that can't be found refuses with exit code 5 and changes nothing. To resolve pins some other way (an internal index, say), set `KNIXL_PIN_RESOLVER` to a command that takes a package name and version and prints a nixpkgs commit.

## Unfree packages

knixl has no built-in switch for unfree packages, so a host allows them the usual NixOS way, with `nixpkgs.config.allowUnfree = true`. The tidiest place for it is a two-line module of your own (see [Writing your own modules](10-writing-modules.md)) whose emit is `set "nixpkgs.config.allowUnfree" #true`; [raw-nix](09-escape-hatches.md) works too.

Pinned packages and packages taken `from=` an input are built with the host's `nixpkgs.config`, so they respect the setting wherever it lives. The trial builds that `install --build` and pin selection run happen outside the host, though, and knixl only allows unfree there when it can see `nixpkgs.config.allowUnfree = true` set unconditionally by a module in the host's generated Nix. If your setting is somewhere knixl can't read (raw-nix, an imported file, or an `allowUnfreePredicate`), set `NIXPKGS_ALLOW_UNFREE=1` when you run `knixl install`.

## Packages from other flakes

In a project whose flake has inputs (see [From KDL to a running machine](04-running-machine.md#other-flake-inputs)), a host can take a package from a second nixpkgs, for example unstable on a stable machine:

```kdl
// knixl.kdl
system {
    state-version "25.11"
    input "nixpkgs" url="github:NixOS/nixpkgs"
    input "nixpkgs-unstable" url="github:NixOS/nixpkgs" rev="<commit>"
}
```

```kdl
// hosts/laptop.kdl
package "handy" from="nixpkgs-unstable"
package "kdePackages.kate" from="nixpkgs-unstable"
```

The input has to be a nixpkgs checkout. It's imported with the host's `nixpkgs.config`, so unfree settings carry over. You can't combine `from=` with `version=` (a version pin already chooses its own nixpkgs commit), and `knixl install` won't touch a package that's already taken `from=` an input.

## Removing a package

Delete the `package` line and run `knixl generate`.
