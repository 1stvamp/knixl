# Reference

Every command, setting and environment variable in one place. The other guide pages explain when you'd use them.

## Project layout

```
homelab/
├── knixl.kdl            project settings (optional)
├── knixl.lock.kdl       the lockfile (written by knixl)
├── hosts/               one .kdl file per machine
├── modules/             your own declarative modules (optional)
└── generated/           everything knixl writes
    ├── hosts/<host>.nix
    ├── installer/<name>.nix
    ├── guest-image/<name>.nix
    ├── flake.nix        with a system block
    └── flake.lock       written by `nix flake lock`, in input mode
```

knixl finds the project by walking up from the current directory to the first directory containing `knixl.lock.kdl` or `hosts/`, so you can run it from anywhere inside the project.

## Commands

Every command except `tui` takes `--json` for machine-readable output: one JSON object per run, with each file's path and state and any warnings (or the validation errors, when validation fails).

### `knixl plan`

Work out what the generated files should be and report each file's state. Writes nothing. Exits 0 unless validation fails; `--detailed-exitcode` makes it exit with the same codes as `check`.

### `knixl check`

The CI gate: like `plan`, but exits non-zero unless every file is clean (and, in input mode, `generated/flake.lock` matches the lockfile). Never writes, never prompts.

### `knixl generate`

Write stale and missing files and update the lockfile. Refuses when a file has been edited by hand, or when a version change would alter the output.

- `--accept-drift`: regenerate hand-edited files, discarding the edits.
- `--prune`: delete orphaned files.

### `knixl upgrade`

Review and apply version changes: a new knixl or module version, a host's new `nixpkgs release`, unpinned flake inputs, and fetched modules. Shows migration notes and what would change; everything that touches the network happens here (and in `install`).

- `--yes`: apply. Without it, `upgrade` only shows what it would do.
- `--refresh`: also move every `nixpkgs release` to the newest commit on its branch (hosts pinned with `rev=` stay put).

### `knixl doc <node>`

Print what a module accepts: its arguments, properties and children, with types and descriptions.

### `knixl install <package>`

Add a package to a host, check it, and regenerate. `<package>` is a nixpkgs attribute name (`ripgrep`, `kdePackages.kate`) or `name@version` to pin a version. Opens the TUI in a terminal.

- `--host <name>`: the host to add it to (default: the host marked `default=#true`, else the only host).
- `--yes`: don't ask for confirmation (and don't open the TUI).
- `--build`: also build the package.
- `--strict`: fail, instead of warning, when Nix isn't available.
- `--no-abi-check`: for a version pin, skip the trial build and use the commit-mix strategy.

### `knixl tui`

Open the interactive interface: install a package, browse modules, create or edit a module. See [The TUI](06-the-tui.md).

## The host node

```kdl
host "<name>" [default=#true] {
    system "<platform>"                  // required, e.g. "x86_64-linux"
    hostname "<name>"                    // networking.hostName, if not the label
    nixpkgs release="<release>" [rev="<commit>"]
    oracle-modules { module "<name>" flake="<ref>" [attr="<attr>"] }
    // module nodes...
}
```

Module nodes are listed in [Using modules](07-modules.md).

## knixl.kdl

All of these are optional.

```kdl
// The project's default nixpkgs: what hosts without their own release are validated
// against, and what installers and guest images build from in an input-free flake.
nixpkgs release="<release>"

// Generate generated/flake.nix.
system {
    state-version "<release>"            // required in a system block
    input "<name>" url="<flake ref>" [rev="<40-character commit>"] {
        follows <input>="<input>"
    }
    formatter "<nixpkgs attr>"           // adds formatter.<system>, for `nix fmt`; needs inputs
    nixpkgs-url "<url>"                  // input-free flakes only: where nixpkgs is fetched from
}

// Out-of-tree NixOS modules to validate against (and, with input=, import into every host).
oracle-modules {
    module "<name>" flake="<ref>" [attr="<attr>"]
    module "<name>" input="<input>" [attr="<attr>"]
}

// Declarative modules from other repositories.
modules {
    module "<name>" flake="<ref>" [path="<dir>"]
}

// Where secret references point; with input=, also wire up sops-nix.
secrets backend="sops-nix"|"agenix" [input="<input>"] {
    default-file "<path>"
    ssh-key-paths "<path>" ...
}

// Installer ISOs and Incus images, built from module nodes like a host.
installer "<name>" [system="<platform>"] { ... }
guest-image "<name>" [system="<platform>"] { ... }
```

## Environment variables

| Variable | What it does |
| --- | --- |
| `KNIXL_FORMATTER` | The formatter to run. Default: the first of `nixfmt`, `nixfmt-rfc-style` found. |
| `KNIXL_OPTIONS_JSON` | A prebuilt NixOS `options.json` to validate every host against. |
| `KNIXL_NIX` | The binary `install` uses to evaluate. Default `nix-instantiate`. |
| `KNIXL_NIX_BUILD` | The binary used for builds (`install --build`, pin trial builds, option sets). Default `nix-build`. |
| `KNIXL_PIN_RESOLVER` | A command `<bin> <name> <version>` that prints a nixpkgs commit, replacing the nixhub lookup. |
| `KNIXL_BASELINE_RESOLVER` | A command `<bin> <release>` that prints a nixpkgs commit, replacing `git ls-remote`. |
| `KNIXL_MODULE_RESOLVER` | Replaces the built-in resolver for flake inputs and fetched modules. |
| `NIXPKGS_ALLOW_UNFREE` | Set to `1` to allow unfree packages in `install`'s builds. |
| `HTTP_PROXY`, `HTTPS_PROXY`, `NO_PROXY` | Proxy settings for the built-in resolvers. |

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Clean, or applied cleanly. |
| 1 | Internal error. |
| 2 | Bad command-line usage. |
| 3 | A generated file was edited by hand. |
| 4 | A version change would alter the output; run `upgrade`. |
| 5 | Validation failed. |
| 6 | Regeneration owed. |

When more than one applies, the order is 5, 3, 4, 6. [Day to day](05-day-to-day.md#exit-codes) has the usual fix for each.
