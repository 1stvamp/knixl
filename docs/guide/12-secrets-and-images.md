# Secrets, installers and images

Three things build on the generated flake from [From KDL to a running machine](04-running-machine.md): referring to secrets without putting them in the repository, building an installer ISO, and building a NixOS image for Incus.

## Secrets

knixl never handles secret values. A module that needs one (Tailscale's auth key, say) takes the *name* of a secret, and the generated Nix points at the path where your secrets tool will put the decrypted file at boot:

```kdl
tailscale {
    auth-key secret="tailscale-authkey"
}
```

```nix
services.tailscale.authKeyFile = config.sops.secrets."tailscale-authkey".path;
```

The encrypted material stays with sops-nix or agenix, out of knixl's sight; knixl doesn't read it, hash it or check the name exists. Choose the tool in `knixl.kdl` (sops-nix is the default):

```kdl
secrets backend="agenix"
```

With agenix the reference becomes `config.age.secrets."tailscale-authkey".path`.

On its own, that only decides what the references look like. Setting up the tool itself (importing its NixOS module, declaring each secret, giving the machine a key) is up to you, as it would be without knixl.

### Letting knixl wire up sops-nix

In a project with flake inputs, knixl can do that setup for sops-nix. Add sops-nix as an input and point `secrets` at it:

```kdl
system {
    state-version "25.11"
    input "nixpkgs" url="github:NixOS/nixpkgs"
    input "sops-nix" url="github:Mic92/sops-nix" {
        follows nixpkgs="nixpkgs"
    }
}

secrets backend="sops-nix" input="sops-nix" {
    default-file "secrets/secrets.yaml"
    ssh-key-paths "/etc/ssh/ssh_host_ed25519_key"
}
```

Every host then imports sops-nix's module, uses `default-file` (relative to `knixl.kdl`) as its default secrets file, decrypts with the machine's SSH host key, and gets a `sops.secrets."<name>"` declaration for every secret its modules refer to. The secrets file has to be committed (encrypted, as sops leaves it), because the flake only sees files git tracks. To have the oracle check `sops.*` options too, also list sops-nix in `oracle-modules` (see [Validation and the oracle](11-validation.md#modules-from-other-flakes)).

## Installer media

An installer is a NixOS live ISO with your own modules on it: SSH with your key, say, and Tailscale, so a fresh machine booted from it is reachable for `nixos-anywhere`. Declare it in `knixl.kdl` with ordinary module nodes:

```kdl
installer "usb" {
    openssh {
        permit-root "prohibit-password"
    }
    user "wes" {
        group "wheel"
        ssh-key "ssh-ed25519 AAAA... wes@desktop"
    }
    tailscale {
        up-flag "--ssh"
    }
}
```

`generate` writes `generated/installer/usb.nix` (NixOS's minimal installation CD plus these modules). With a `system` block, the flake also gets a package for it, so you can build the ISO:

```sh
nix build ./generated#usb-iso
```

Add `system="aarch64-linux"` to the `installer` node for another platform. In an input-free flake (no `input` lines), images build from the project's own nixpkgs, so declare one at the top of `knixl.kdl` with `nixpkgs release="25.11"` and run `knixl upgrade --yes`; until then the flake leaves them out. A live ISO has nothing to decrypt secrets with, so `secret=` references don't work here; supply anything secret when you build the image, e.g. from a module you `import` (paths are relative to `knixl.kdl`) that reads an environment variable, built with `--impure`.

## Images for Incus

`guest-image` is the same idea for a NixOS system you'll run as an Incus container:

```kdl
guest-image "llm" {
    openssh { }
    package "htop"
}
```

`generate` writes `generated/guest-image/llm.nix`, and the flake gets two packages, the root filesystem and Incus's metadata:

```sh
nix build ./generated#llm-lxc -o rootfs
nix build ./generated#llm-lxc-metadata -o metadata
incus image import metadata/tarball/*.tar.xz rootfs/tarball/*.tar.xz --alias llm
```

knixl stops at building the image; importing and launching it is up to you. (This is different from the [`guest`](07-modules.md#guest) module, which runs a NixOS container managed by the host's own NixOS config.)
