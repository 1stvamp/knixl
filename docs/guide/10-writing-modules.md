# Writing your own modules

A module of your own gives a setting the same treatment as the built-in ones: a KDL shape, validation of what's written, a `knixl doc` page, and checked output. Most modules are *declarative*: a single KDL file that says which node it handles, what that node accepts, and which NixOS options to set from it. No Rust and no rebuilding knixl.

This page builds one for Syncthing, running as one user and sharing a few folders. (The [TUI's New module screen](06-the-tui.md#new-module) builds the same file interactively, with live validation, if you'd rather not write it by hand.)

## Where modules live

Put each module in its own directory under `modules/`, at the project root next to `hosts/`:

```
homelab/
├── hosts/
├── modules/
│   └── syncthing/
│       └── knixl-module.kdl
└── knixl.kdl
```

knixl picks up everything in `modules/` on its next run.

## The module file

`modules/syncthing/knixl-module.kdl`:

```kdl
module name="syncthing" version="1.0.0" {
    summary "Syncthing for one user, with the folders it shares."
    claims-node "syncthing"

    schema {
        arg "user" type="string" required=#true \
            doc="The user Syncthing runs as."
        child "open-firewall" type="bool" \
            doc="Open Syncthing's sync and discovery ports."
        child "folder" repeated=#true doc="A folder to share." {
            arg "id" type="string" required=#true doc="The folder ID, the same on every device."
            prop "path" type="string" required=#true doc="Where the folder lives on this machine."
        }
    }

    emit {
        set "services.syncthing.enable" #true
        set "services.syncthing.user" "{user}"
        set "services.syncthing.dataDir" "/home/{user}"
        when-flag "open-firewall" {
            set "services.syncthing.openDefaultPorts" #true
        }
        for-each "f" in "folder" {
            set "services.syncthing.settings.folders.{f.id}.path" "{f.path}"
        }
    }
}
```

`claims-node` is the node name you'll write in host files. `version` is recorded in the lockfile (more on that below), and `summary` is the first line of `knixl doc syncthing`.

A host can now use it:

```kdl
host "laptop" {
    system "x86_64-linux"

    syncthing "wes" {
        open-firewall #true
        folder "notes" path="/home/wes/notes"
        folder "photos" path="/home/wes/photos"
    }
}
```

and `knixl generate` writes:

```nix
  services.syncthing.enable = true;
  services.syncthing.user = "wes";
  services.syncthing.dataDir = "/home/wes";
  services.syncthing.openDefaultPorts = true;
  services.syncthing.settings.folders."notes".path = "/home/wes/notes";
  services.syncthing.settings.folders."photos".path = "/home/wes/photos";
```

## The schema: what the node accepts

The `schema` block describes the node's shape. knixl checks every use of the node against it, refuses anything it doesn't declare, and builds `knixl doc` from it, so give each field a `doc`.

- `arg "<name>"` is a positional value on the node (`syncthing "wes"`).
- `prop "<name>"` is a `key=value` on the node.
- `child "<name>"` is a nested node. With a `type`, it holds one value (`timezone "Europe/London"`); a `type="bool"` child is a flag you switch on with `open-firewall #true`. Without a type, it can have args and props of its own, declared inside it (like `folder` above).

Each takes `type` (`"string"`, `"int"` or `"bool"`; anything else is treated as a string), `required=#true` and, for children, `repeated=#true` to allow more than one.

An optional single value is usually written as a repeated child, used with `for-each`, and documented as "at most one": the template then only writes the option when the child is there. The built-in `openssh` module's `permit-root` works this way.

## The emit block: what it sets

Each statement in `emit` writes NixOS options:

- `set "<path>" <value>` sets one option. Parts of the path or value in braces are filled in from the node: `{user}` is the arg, `{f.id}` a field of the loop variable `f`.
- `when-flag "<child>" { ... }` includes its statements only when that bool child is set.
- `for-each "<var>" in "<child>" { ... }` repeats its statements for every instance of a repeated child.
- `list "<path>" from "<child>" { set "<attr>" <value> ... }` builds a list of attribute sets, one per child; `list-opt` does the same but writes nothing when there are no children.
- `when-config "<condition>" { ... }` wraps its options in `lib.mkIf`, for a condition only known when Nix evaluates the system (e.g. `"config.services.postgresql.enable"`).

Values can be literals (`#true`, `16`, `"text"`), strings with `{lookups}` in them, or one of a few special forms:

- `(collect)"<child>"` makes a list from a repeated child's values. `(collect-opt)` is the same but leaves the option out when the list would be empty, which keeps the NixOS default.
- `(list)"a" "b"` writes a list of the values that follow it on the line, e.g. `(list)"/mnt/{share.tag}"` for an option that wants a list even when there's only one entry.
- `(scalar)"{lookup}"` writes a bool or number as itself, where a plain `"{lookup}"` would make it a string.
- `(secret)"<name>"` points at a decrypted secret's path (see [Secrets, installers and images](12-secrets-and-images.md)).
- `(indent-str)#""" ... """#` writes a multi-line Nix string.

The template grammar in the design specs ([knixl.dev/design/template-grammar](/design/template-grammar/)) has every detail, written for contributors.

## What a declarative module can't do

Templates deliberately stay simple. They can't compute one option from several inputs, build attribute sets with arbitrary keys several levels deep, or emit references to packages (`pkgs.zlib`; a lookup always becomes a string). When a module needs one of those, the options are a hand-written module brought in with [`import`](09-escape-hatches.md#a-whole-module-import), or a built-in module written in Rust as part of knixl itself.

## Validation while you write

When knixl loads a module it checks the template against the schema: a `{lookup}` naming a field the schema doesn't declare, or a loop over a child that isn't repeated, fails straight away with the module's file named. Every option path the module sets is also checked against the NixOS option set when [the oracle](11-validation.md) is active, so a misspelt option fails at generate time.

## Changing a module later

Editing a module and regenerating is routine: the files that use it show as `stale`, and `generate` updates them. Bumping its `version` is the signal that the output changed in a way users should review. knixl then treats it like any other version change: `generate` refuses with exit code 4, and `knixl upgrade` shows the change and records the new version.

## Using modules from other projects

A module can live in someone else's flake and be shared between projects. Declare it in `knixl.kdl`:

```kdl
modules {
    module "syncthing" flake="github:example/knixl-modules" path="syncthing"
}
```

`path` is the directory inside that repository holding `knixl-module.kdl` (leave it out if it's at the root). `knixl upgrade` resolves the flake to a commit, downloads the module and records its hash in the lockfile; after that, `generate` works offline from the cached copy and refuses if the cached file doesn't match its recorded hash.

When two sources define the same node, your `modules/` wins over a fetched module, and both win over the KDL modules bundled with knixl ([which ones those are](07-modules.md#modules-from-elsewhere)), so you can replace a bundled module with your own version of it. knixl reports every module that's hidden this way. The modules that are part of knixl's code always win.
