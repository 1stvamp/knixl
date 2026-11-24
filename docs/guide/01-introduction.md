# What knixl is

knixl turns short, opinionated KDL descriptions of your NixOS machines into ordinary NixOS modules, which you commit alongside the KDL and build with Nix as usual.

You write this:

```kdl
host "web" {
    system "x86_64-linux"

    web-service "example.com" {
        upstream "http://127.0.0.1:3000"
        acme email="ops@example.com"
        hardened #true
    }
}
```

and knixl writes `generated/hosts/web.nix`: nginx switched on, a virtual host proxying to your app, TLS and the recommended proxy settings on, ACME wired up for the certificate, and a set of security headers. It's plain Nix that you can read, review in a pull request and `import` from anywhere, and every line of it came from those few lines of KDL.

## Why you might want it

NixOS modules are powerful, but a typical host ends up as a long file of option paths, and the knowledge of which options belong together (the nginx settings that go with a reverse proxy, the ZFS settings a pool needs) lives in your head or in old commits. knixl moves that knowledge into named modules, so a host file describes the machine (a web server for `example.com`, a NAS with a pool called `tank`) and the modules hold the option-level detail.

A few things come with that:

- **The output is checked.** KDL that no module understands is refused outright, so a typo never quietly drops out of your config. Once a host names its nixpkgs release, every option path knixl writes is also checked against the real NixOS option set for it (see [Validation and the oracle](11-validation.md)), so a misspelt or renamed option fails when you generate instead of at `nixos-rebuild` time.
- **The output is locked.** knixl records a hash of every generated file in `knixl.lock.kdl`. Regenerating from the same KDL with the same versions gives the same bytes, and `knixl check` proves it in CI.
- **Upgrades wait for you.** If a new knixl release would change your generated Nix, `generate` refuses and points you at `knixl upgrade`, which shows you what changed before it writes anything.
- **Hand edits are caught.** If someone edits a generated file directly, knixl notices and refuses to overwrite their change until you decide what to do with it.
- **You can work interactively.** `knixl tui` lets you add packages with a live preview of the generated Nix, browse the modules and their options, and build your own modules with live validation (see [The TUI](06-the-tui.md)).

## What it isn't

knixl doesn't replace Nix or the NixOS module system. It writes NixOS modules, and Nix builds them. It also only works in one direction: if you edit the generated Nix, knixl can't turn that back into KDL. Changes go in the KDL, or in a hand-written module that sits beside the generated one (see [When knixl doesn't cover it](09-escape-hatches.md)). [Why not just write Nix?](01a-why-not-just-nix.md) covers when plain Nix is the better choice, and [knixl and the alternatives](01b-alternatives.md) how it compares with nearby tools.

It doesn't try to model all of NixOS either. The built-in modules cover the common ground (users, SSH, the firewall, disks, ZFS, Tailscale, nginx, Incus, packages and so on), and for everything else you can drop in raw Nix, import a module you wrote by hand, or write a small knixl module of your own.

## The model in brief

- **KDL is the source of truth.** You edit the files under `hosts/`. The files under `generated/` are derived from them, and you commit both.
- **The lockfile pins everything that affects the output** (the knixl, module and formatter versions, and each host's nixpkgs), so the same KDL gives the same files on any machine.
- **KDL edits apply straight away, version changes wait for review.** `knixl generate` applies your KDL edits. A version change goes through `knixl upgrade`, which shows you the difference first.
- **You choose how much knixl owns.** It can write just the host modules and leave the rest of your flake to you, or it can also write a complete `flake.nix` you can deploy straight from (see [From KDL to a running machine](04-running-machine.md)).

## Where to go next

If you're weighing knixl up, read [Why not just write Nix?](01a-why-not-just-nix.md) and [knixl and the alternatives](01b-alternatives.md). If you're new, start with [Installing](02-installing.md) and then [Your first host](03-first-host.md), which builds a small project from an empty directory. If you already have a working project, [Day to day](05-day-to-day.md) covers the everyday commands and what to do when `check` fails, and [Reference](13-reference.md) lists every command, flag and setting.
