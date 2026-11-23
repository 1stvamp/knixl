# From KDL to a running machine

[Your first host](03-first-host.md) generated a NixOS module for `laptop`. To build and switch to it, something has to wrap that module in a NixOS system: a `flake.nix` with a `nixosConfigurations.laptop` entry, pinned to a nixpkgs. You can write that flake yourself and import the module (the end of the last page shows how), or you can have knixl generate it. This page does the second.

## Give the host a nixpkgs release

A bootable system needs to know which nixpkgs it's built from. Each host says which release it tracks with a `nixpkgs` child:

```kdl
host "laptop" {
    system "x86_64-linux"
    nixpkgs release="25.11"

    openssh {
        permit-root "no"
    }
    // ...
}
```

The release name is only metadata. knixl resolves it to an exact nixpkgs commit (the tip of the `nixos-25.11` branch at that moment) and records the commit in the lockfile, so the host doesn't move until you ask it to. If you'd rather name the commit yourself, say the one a running machine was built from, add it: `nixpkgs release="unstable" rev="<commit>"`.

## Ask for a flake

At the root of the project, next to `hosts/`, create `knixl.kdl`. This is the project-wide settings file:

```kdl
system {
    state-version "25.11"
    input "nixpkgs" url="github:NixOS/nixpkgs"
}
```

The `system` block tells knixl to write `generated/flake.nix`. `state-version` becomes every host's `system.stateVersion` (the NixOS release whose defaults the machine was installed with: set it once and leave it), and `input "nixpkgs"` makes nixpkgs a flake input.

`state-version` is one value for the whole project. If a machine was installed on an older release, keep its own value with an override module (see [Changing what a module set](09-escape-hatches.md#changing-what-a-module-set)) containing `system.stateVersion = lib.mkForce "24.05";`.

## Resolve, generate, lock

```sh
knixl upgrade --yes
knixl generate
```

`upgrade` is the command that does anything involving the network or a version: here it resolves `release="25.11"` to a commit and records it. Run it without `--yes` first if you'd like to see what it would do. This is also the moment to set up validation for that commit, which takes one command and a minute's wait: see [Turning it on](11-validation.md#turning-it-on). `generate` then writes the flake:

```nix
{
  description = "knixl-generated system";
  inputs = {
    "nixpkgs" = {
      url = "github:NixOS/nixpkgs/b6018f87da91d19d0ab4cf979885689b469cdd41";
    };
  };
  outputs =
    { nixpkgs, ... }@inputs:
    {
      nixosConfigurations = {
        "laptop" = nixpkgs.lib.nixosSystem {
          system = "x86_64-linux";
          specialArgs = { inherit inputs; };
          modules = [
            ./hosts/laptop.nix
            { system.stateVersion = "25.11"; }
          ];
        };
      };
    };
}
```

The nixpkgs input points at the exact commit from the lockfile. Nix also wants its own `flake.lock` beside the flake. Nix only sees files git knows about, so add the generated files first, then lock:

```sh
git add generated
(cd generated && nix flake lock)
```

Commit `generated/flake.lock` with everything else. knixl doesn't write that file, but `knixl check` reads it and fails if it's missing or pins a different nixpkgs commit than `knixl.lock.kdl`, so the two can't drift apart.

## Hardware and boot

A real machine needs its hardware configuration and a boot loader, which differ for every machine. Keep `nixos-generate-config`'s `hardware-configuration.nix` in the project and import it from the host:

```kdl
host "laptop" {
    system "x86_64-linux"
    nixpkgs release="25.11"

    import "laptop-hardware.nix"

    os {
        boot-loader "systemd-boot"
        efi-can-touch-variables #true
        timezone "Europe/London"
        locale "en_GB.UTF-8"
    }
    // ...
}
```

The `import` path is relative to the host's KDL file (so this one lives at `hosts/laptop-hardware.nix`) and has to stay inside the project. For disk layouts managed declaratively, see the `disko` module in [Using modules](07-modules.md).

## Deploy

```sh
sudo nixos-rebuild switch --flake ./generated#laptop
```

For a fresh machine, `nixos-anywhere --flake ./generated#laptop root@<address>` installs it over SSH (boot it from an [installer ISO](12-secrets-and-images.md#installer-media) first). The hardware file doesn't exist yet at that point; `nixos-anywhere`'s `--generate-hardware-config nixos-generate-config ./hosts/laptop-hardware.nix` writes it from the target before installing, and you commit it afterwards. From here on the loop is: edit the KDL, `knixl generate`, commit, rebuild.

## More hosts

Add another file under `hosts/` and it gets its own `nixosConfigurations` entry. In this mode (with `input` nodes) every host builds from the one nixpkgs input, so every host has to declare the same release; knixl refuses a project whose hosts resolve to different commits.

If your machines need to sit on different releases (moving a fleet from 25.05 to 25.11 one host at a time, say), leave the `input` line out:

```kdl
system {
    state-version "25.11"
}
```

knixl then writes a flake with no inputs, which fetches each host's nixpkgs commit itself, so every host can be on its own release. The cost is that you can't add other flake inputs (so no disko, sops-nix or home-manager wiring). If you can keep every host on one release, input mode is the better choice.

## Other flake inputs

Input mode is also how you bring in modules from other flakes, such as disko for disk layouts or sops-nix for secrets. Declare them beside nixpkgs, pinned with `rev=`:

```kdl
system {
    state-version "25.11"
    input "nixpkgs" url="github:NixOS/nixpkgs"
    input "disko" url="github:nix-community/disko" rev="<commit>" {
        follows nixpkgs="nixpkgs"
    }
}
```

An input without `rev=` is resolved to its current commit by `knixl upgrade`; a URL with a branch or tag in it is refused, so pin with `rev=` (a full 40-character commit) instead. Declaring an input only makes it available. To add its NixOS module to every host, list it in `oracle-modules`:

```kdl
oracle-modules {
    module "disko" input="disko" attr="disko"
}
```

The name comes from the block's other job: knixl also validates against the options that module declares (see [Validation and the oracle](11-validation.md#modules-from-other-flakes)). With `input=`, one line does both, and this is how the `disko` and `home-manager` modules get the NixOS modules they rely on. (Without a generated flake, you import those modules in your own flake, as you would without knixl.)

Inputs are also available to your own Nix as a module argument (`{ inputs, ... }:` in a file you `import`), and a host can take packages from them with `package "<name>" from="<input>"` (see [Packages and pins](08-packages.md)). [Secrets, installers and images](12-secrets-and-images.md) covers sops-nix, installer ISOs and Incus images, which build on the same flake.
