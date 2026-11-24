# Why not just write Nix?

You can, and if your hosts are already a set of well-factored modules that you're happy with, you probably should carry on. knixl is for the case where each host has ended up as a long file of option paths, and the knowledge of which options belong together lives in your head or in old commits.

## Isn't this another abstraction that will leak?

It will, which is why the output is ordinary Nix that you commit. Each host becomes a plain NixOS module under `generated/`, formatted with nixfmt, that you can read, review in a pull request and `import` from anywhere. When a module doesn't cover something you use `raw-nix`, `import` a hand-written module, or add a sibling module with `lib.mkForce` (see [When knixl doesn't cover it](09-escape-hatches.md)).

If you decide knixl isn't for you, delete the KDL and keep `generated/`: it's just your config.

## Couldn't I do this with my own NixOS modules and options?

Mostly, yes. A module with a few options gives you the same presets, evaluated by Nix. What knixl adds happens before Nix runs:

- **Typos refuse.** A KDL node no module understands exits 5, and once a host has its NixOS option set, every option path knixl writes is checked against the real options for that host's nixpkgs (see [Validation and the oracle](11-validation.md)).
- **The diff is the change.** A one-line KDL edit appears in review as the exact option lines it adds or removes, because the generated Nix is committed.
- **Upgrades wait.** A new knixl or module version can't change your output until you've looked at it in `knixl upgrade`.
- **Hand edits are caught.** Edit a generated file and knixl calls it drifted, and won't overwrite it (exit 3) unless you pass `--accept-drift` (see [When a file has drifted](05-day-to-day.md#when-a-file-has-drifted)).

## Why commit generated code?

So that it can be reviewed and checked. `knixl check` recomputes everything in CI and fails if a generated file is stale, drifted or invalid, and a reviewer sees the Nix that will be built (the reasoning is in [ADR 0002](../adr/0002-emit-source-not-values.md)).

## Why KDL?

KDL has nodes, arguments and properties, which map well onto "a thing, with a name, with some settings", and the official Rust parser keeps source spans and formatting. That gives errors that point at your file, and lets `knixl install` edit a host file without disturbing your comments.

## What won't it do?

- It only goes one way. Edits to generated Nix can't be turned back into KDL.
- It doesn't model all of NixOS. The built-in modules cover the common ground, and the rest is raw Nix or a module you write.
- It has no functions, loops or includes across hosts: each host file stands on its own. What hosts share is modules (the built-in ones and [your own](10-writing-modules.md)), the project-wide settings in `knixl.kdl` (such as flake inputs and the modules added to every host), and hand-written Nix that several hosts `import`. If your config relies on Nix-language abstraction to stamp out many similar machines, knixl will feel restrictive.
- A version-pinned package is fetched with `builtins.fetchGit` at evaluation time, outside your flake's inputs and `flake.lock` (see [Packages and pins](08-packages.md#pinning-a-version)).
- It only writes NixOS configuration. There's no nix-darwin support, and home-manager only works through its NixOS module (the [`home-manager`](07-modules.md#home-manager) node), so there's no standalone home-manager configuration.

## Who is it for?

People who run a handful of NixOS machines and would rather describe each one in a few lines of KDL than maintain a long file of option paths.
