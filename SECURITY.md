# Security policy

## Reporting a vulnerability

Please do not open a public GitHub issue for a security problem. Instead, email
wes@1stvamp.org with a description of the issue, steps to reproduce, and its
impact if you can assess it.

You should get an acknowledgement within 5 working days. We will keep you
updated as the issue is triaged and fixed, and will credit you in the fix
(commit message and, where applicable, release notes) unless you ask
otherwise.

## Supported versions

knixl is pre-1.0 (`0.x`). Only the latest published release on crates.io is
supported with security fixes. There is no long-term support branch: upgrade
to the latest release to pick up a fix.

## Scope

knixl generates and writes files (`generated/`, `knixl.lock.kdl`) based on KDL
you provide, and can invoke a local Nix and formatter binary. Relevant reports
include, for example, a KDL input that causes knixl to write outside the
project root, or a lockfile that can be manipulated to bypass drift detection.
Vulnerabilities in generated Nix itself (e.g. a hardening default that is not
as strong as documented) are also in scope.

Issues in the oracle's nixpkgs snapshot, or in the pinned formatter, are
upstream concerns and should be reported to nixpkgs or the formatter project
directly.
