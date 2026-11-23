# When knixl doesn't cover it

knixl's modules cover the common ground, but sooner or later you'll want an option no module sets, or want to change a value a module chose. There are four ways to do it, and which one fits depends on how big the change is and whether you'll want it again.

| You want to | Use |
| --- | --- |
| Set an option or two that no module handles | [`raw-nix`](#a-few-lines-raw-nix) |
| Bring in a whole hand-written module (hardware config, a service with its own derivation) | [`import`](#a-whole-module-import) |
| Change a value a knixl module set | [an override module](#changing-what-a-module-set) |
| Do the same thing on several hosts, or give it a tidy KDL shape | [your own module](#something-youll-reuse-your-own-module) |

What you shouldn't do is edit the generated file: knixl spots the edit, refuses to overwrite it, and the change is lost the moment someone accepts the drift.

## A few lines: raw-nix

```kdl
raw-nix {
    #"""
    systemd.services.nginx.serviceConfig.MemoryMax = "512M";
    """#
}
```

The text is copied into the host's generated module as written (the `#"""` ... `"""#` is KDL's raw multi-line string, so you don't need to escape anything). It's part of the generated file, so it's hashed and covered by drift detection like everything else.

The catch is that knixl doesn't read it. The oracle doesn't validate option paths inside raw-nix, and features that look at the generated config (like the unfree check in `knixl install`) can't see it either. A typo in raw-nix only surfaces when Nix evaluates the system, so it's best kept to the odd line.

Two limits: raw-nix can't add its own `imports = [ ... ];` (use `import` instead), and it's spliced into the module's body, so it can only contain option assignments.

## A whole module: import

```kdl
host "laptop" {
    system "x86_64-linux"
    import "laptop-hardware.nix"
}
```

`import` adds a hand-written NixOS module to the host's `imports`. The path is relative to the host's KDL file, must stay inside the project (a flake can't see files outside its repository) and can be a `.nix` file or a directory with a `default.nix`.

Use it for anything that's naturally a whole module: `hardware-configuration.nix`, a service that needs a `let` block or its own package, something you're copying in from an existing config. In a project with flake inputs, the module can take `inputs` as an argument (`{ inputs, pkgs, ... }:`) to reach anything those inputs provide.

knixl doesn't hash an imported file or validate its options, so changes to it don't show up in `knixl check`; review them in git like any other source file.

## Changing what a module set

Modules set ordinary NixOS options, so the NixOS module system's own priorities work on them. To change a value a knixl module chose, put the new value in a module of your own with `lib.mkForce` (or `lib.mkDefault`, `lib.mkAfter` and so on, as the option suits), and `import` it:

```nix
# hosts/laptop-overrides.nix
{ lib, ... }:
{
  services.openssh.settings.PasswordAuthentication = lib.mkForce true;
}
```

```kdl
host "laptop" {
    system "x86_64-linux"
    openssh { }
    import "laptop-overrides.nix"
}
```

This is also the right home for a fix that turned up as [drift](05-day-to-day.md#when-a-file-has-drifted): move the hand edit into an override file, then `knixl generate --accept-drift`.

## Something you'll reuse: your own module

If you find yourself writing the same raw-nix on several hosts, or you'd like a setting to have a proper KDL shape with validation and `knixl doc` output, make it a module. A declarative module is a short KDL file in `modules/` that declares what the node accepts and which options it sets, and once it exists it works exactly like the built-in ones. [Writing your own modules](10-writing-modules.md) shows how, and [the TUI](06-the-tui.md#new-module) can build one for you.
