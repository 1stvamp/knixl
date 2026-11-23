# Using modules

A host file is mostly module nodes: `openssh { ... }`, `user "wes" { ... }`, `zfs "8425e349" { ... }`. Each node is handled by a module that knows which NixOS options to set for it. knixl ships with a set of modules built into the binary, so there's nothing to install or copy before you use them.

For the exact fields any module accepts, run `knixl doc <node>` (or open **Browse modules** in [the TUI](06-the-tui.md)). This page is the overview: what each module is for, and a short example to start from. Every example here generates as shown; the [Examples](/examples/web/) section of the site has complete hosts with the Nix they produce.

| Module | For |
| --- | --- |
| [`os`](#os) | Core machine settings: boot loader, kernel, time zone, locale, sysctls, nix settings, system packages. |
| [`user`](#user) | A login user with groups and SSH keys. |
| [`openssh`](#openssh) | A hardened SSH server. |
| [`firewall`](#firewall) | Open ports, trusted interfaces, the nftables backend. |
| [`tailscale`](#tailscale) | Tailscale, with its auth key from a secret. |
| [`package`](#package) | A package in `environment.systemPackages`. |
| [`zfs`](#zfs) | ZFS support and pools. |
| [`disko`](#disko) | Declarative disk partitioning and formatting. |
| [`web-service`](#web-service) | An nginx reverse proxy with TLS. |
| [`security-headers`](#security-headers) | One shared set of security headers for several nginx sites. |
| [`postgres`](#postgres) | A PostgreSQL server. |
| [`backups`](#backups) | Scheduled restic backups. |
| [`incus`](#incus) | An Incus container and VM host. |
| [`guest`](#guest) | A NixOS container running on the host. |
| [`nix-ld`](#nix-ld) | Running prebuilt, dynamically linked binaries. |
| [`home-manager`](#home-manager) | home-manager for one user. |
| [`import`](#import), [`raw-nix`](#raw-nix) | Escape hatches for Nix knixl doesn't model. |

The `host` node itself takes a few settings that aren't modules: `system` (the platform, required), `hostname` (when it should differ from the host's label), `nixpkgs release=` (see [From KDL to a running machine](04-running-machine.md)) and `oracle-modules` (see [Validation and the oracle](11-validation.md)). Add `default=#true` to one host (`host "laptop" default=#true { ... }`) and commands that need a host, like `knixl install`, use it when you don't name one.

## os

The settings every machine has. Each child is optional; set only what you want to differ from the NixOS default.

```kdl
os {
    boot-loader "systemd-boot"
    efi-can-touch-variables #true
    kernel-package "linuxPackages_6_18"
    timezone "Europe/London"
    locale "en_GB.UTF-8"
    sysctl "net.ipv4.ip_forward"=1 "vm.swappiness"=10
    kernel-module "br_netfilter"
    experimental-feature "nix-command"
    experimental-feature "flakes"
    trusted-user "wes"
    nix-setting "max-jobs"=4
    system-package "vim"
    session-variable "EDITOR"="vim"
    tmpfiles-rule "/var/lib/bench" type="d" mode="0755" user="wes" group="users"
}
```

`state-version` sets `system.stateVersion`; leave it out when `knixl.kdl` has a `system` block, which sets it for every host. `mutable-users #false` makes NixOS the only source of users and passwords.

## user

A normal login user.

```kdl
user "wes" {
    description "Wes Mason"
    group "wheel"
    ssh-key "ssh-ed25519 AAAA... wes@desktop"
    hashed-password "$6$..."
}
```

`hashed-password` takes a hash from `mkpasswd`; there's no way to give a plain password. On its own it only sets the initial password; add `mutable-users #false` under `os` if the declared one should always win.

## openssh

An SSH server with password and keyboard-interactive logins switched off.

```kdl
openssh {
    port 22
    port 2222
    permit-root "prohibit-password"
}
```

Without `port`, NixOS listens on 22.

## firewall

The NixOS firewall. NixOS turns it on by default; this module opens what you need.

```kdl
firewall {
    nftables #true
    allowed-tcp-port 443
    allowed-udp-port 51820
    allowed-tcp-range from=1714 to=1764
    trusted-interface "tailscale0"
    allow-ping #false
}
```

`nftables #true` switches the backend from iptables to nftables. Traffic on a `trusted-interface` bypasses the firewall entirely. `enable`, `allow-ping` and `log-refused-connections` take `#true` or `#false` and are only written when you set them. Ports opened here add to any opened by other modules, such as Tailscale's `open-firewall`.

## tailscale

```kdl
tailscale {
    open-firewall #true
    up-flag "--ssh"
    set-flag "--operator=wes"
    auth-key secret="tailscale-authkey"
}
```

`auth-key` names a secret holding a Tailscale auth key, so the machine joins your tailnet on first boot (see [Secrets, installers and images](12-secrets-and-images.md)). Without it, you log in interactively. NixOS only passes `up-flag`s to `tailscale up` when it logs in with the auth key; `set-flag`s are applied with `tailscale set` every time the service starts, so settings that should always hold (like `--operator`) belong there.

## package

```kdl
package "ripgrep"
package "htop" version="3.2.1"
package "kdePackages.kate"
```

Adds a package to `environment.systemPackages`. Usually you'd let `knixl install` write these lines for you; [Packages and pins](08-packages.md) covers version pins and taking packages from other flakes.

## zfs

```kdl
zfs "8425e349" {
    auto-scrub #true
    extra-pool "tank"
    arc-max-bytes 8589934592
}
```

The argument is the machine's `networking.hostId`, which ZFS needs to import pools safely: eight hex digits, unique per machine (`head -c4 /dev/urandom | od -A none -t x4` makes one). `extra-pool` imports a pool at boot, and `arc-max-bytes` caps the memory ZFS uses for its cache.

## disko

A declarative disk layout, which [disko](https://github.com/nix-community/disko) uses to partition and format disks (with `nixos-anywhere`, for example) and to set up the matching `fileSystems`.

```kdl
disko {
    disk "main" device="/dev/nvme0n1" {
        partition "ESP" size="512M" type="EF00" {
            filesystem format="vfat" mountpoint="/boot" {
                mount-option "umask=0077"
            }
        }
        partition "crypt" size="100%" {
            luks name="cryptroot" password-file="/tmp/secret.key" {
                filesystem format="ext4" mountpoint="/"
            }
        }
    }
}
```

A partition holds one of `filesystem`, `swap`, `luks` (which holds a filesystem of its own) or `zfs pool="..."`, and `zpool` blocks declare the pools. On `luks`, `password-file` is read while formatting, and `key-file` is the key used to unlock at boot. disko's own NixOS module has to be part of the system too. In a generated flake, declare disko as an input and list it in `oracle-modules`, which adds it to every host (see [Other flake inputs](04-running-machine.md#other-flake-inputs)); with your own flake, import `disko.nixosModules.disko` there.

## web-service

An nginx virtual host proxying to an application, with TLS.

```kdl
web-service "example.com" {
    upstream "http://127.0.0.1:3000"
    acme email="ops@example.com"
    hardened #true
    alias "www.example.com"
    location "/api" upstream="http://127.0.0.1:4000"
}
```

With `acme`, the certificate comes from Let's Encrypt. `hardened` adds recommended security headers.

## security-headers

```kdl
security-headers {
    vhost "a.example.com"
    vhost "b.example.com"
}
```

Applies one set of security headers to several nginx virtual hosts. knixl writes the header block once and refers to it from each site.

## postgres

```kdl
postgres version=16 {
    listen-tcp #true
    database "app"
}
```

PostgreSQL with a hardened base configuration (local socket connections only, unless you add `listen-tcp`). `database` makes sure a database exists.

## backups

```kdl
backups {
    repo "s3:https://s3.example.com/backups/db"
    schedule "daily"
    when "config.services.postgresql.enable"
}
```

Scheduled restic backups. `schedule` is a systemd calendar expression. `when` is a Nix condition: the backup is only configured when it's true, which suits a backup shared between hosts that don't all run the service.

## incus

```kdl
incus {
    ui
    https-address-from-interface "tailscale0"
    firewall {
        trust-interface "incusbr0"
        open-api-on "tailscale0"
    }
    storage-pool "default" driver="zfs" source="rpool/incus"
    network "incusbr0" type="bridge" ipv4="auto" nat="true"
    profile "default" pool="default" network="incusbr0"
}
```

An Incus host: the daemon, the web UI, and the initial storage pool, network and profile. `https-address-from-interface` binds the API to an interface's address when the machine boots, which suits an address handed out by Tailscale or DHCP.

## guest

A NixOS system container (`containers.<name>`) running on this host, configured with ordinary knixl modules.

```kdl
guest "sandbox" {
    autostart #true
    private-network #true
    host-address "10.100.0.1"
    local-address "10.100.0.2"
    bind-mount "/data" host-path="/srv/sandbox" read-only=#true
    config {
        openssh {
            permit-root "no"
        }
    }
}
```

Anything inside `config` is generated as the container's own configuration. If you'd rather have an image to run under Incus, see `guest-image` in [Secrets, installers and images](12-secrets-and-images.md).

## nix-ld

```kdl
nix-ld {
    library "stdenv.cc.cc.lib"
    library "zlib"
}
```

Lets binaries built for other Linux distributions (a downloaded release binary, a rustup toolchain) run on NixOS, by providing the libraries they expect. List what they need; `ldd` on the binary tells you.

## home-manager

```kdl
home-manager "wes" state-version="24.11" {
    session-var "EDITOR" "nvim"
    program "git"
    program "fish"
}
```

Manages one user's home with home-manager's NixOS module. `state-version` is required. Session variables only reach shells that home-manager manages. As with disko, home-manager's own NixOS module has to be part of the system: in a generated flake, add it as an input and list it in `oracle-modules`; with your own flake, import it there.

## import

```kdl
import "laptop-hardware.nix"
```

Imports a NixOS module you wrote by hand. See [When knixl doesn't cover it](09-escape-hatches.md).

## raw-nix

```kdl
raw-nix {
    #"""
    systemd.services.nginx.serviceConfig.MemoryMax = "512M";
    """#
}
```

Copies Nix into the generated module as written. See [When knixl doesn't cover it](09-escape-hatches.md).

## Settings shared by several hosts

knixl has no include or inheritance between host files, so a block every machine needs (your user, SSH, nix settings) is either repeated in each host or, better, wrapped in a small module of your own: a `base` module whose emit sets the shared options, written once in `modules/base/` and added to each host as `base`. See [Writing your own modules](10-writing-modules.md).

## Modules from elsewhere

You aren't limited to these. A `modules/` directory in your project holds modules you've written (see [Writing your own modules](10-writing-modules.md)), and `knixl.kdl` can declare modules published in other people's flakes.

Some of knixl's own modules are written the same way, as KDL files bundled with knixl: `openssh`, `user`, `firewall`, `tailscale`, `zfs`, `web-service`, `security-headers` and `home-manager`. A module of yours with the same node name replaces one of those (knixl tells you it's doing so), which is how you'd change one to suit you. The others (`os`, `disko`, `incus`, `guest`, `nix-ld`, `postgres`, `backups`, `package`, `raw-nix`, `import` and `host` itself) are part of knixl's code and can't be replaced.
