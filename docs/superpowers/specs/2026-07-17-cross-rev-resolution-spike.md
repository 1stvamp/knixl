# Spike: cross-rev resolution via overrideAttrs / flakes (#23)

Date: 2026-07-17
Status: investigation only, no implementation
Issue: #23
Relates to: docs/adr/0005-package-version-pinning.md

This is a time-boxed investigation, not a design to build. #23 asks whether the two
alternatives ADR 0005 rejected for the first cut (`overrideAttrs` and flake-input-per-package)
should be revisited. The remit from the backlog is explicit: "revisit if the historical-commit
approach proves insufficient." So the first question is whether it has, and the answer shapes
everything else.

## The problem being solved

`knixl install pkg@version` needs a specific version of one package on one host. Plain
nixpkgs has no per-package version selector: one commit ships one version of each package,
and there is no `pkgs.<name>."1.2.3"`. Whatever approach we pick has to bridge that gap, and
every approach that pulls a package built against a different nixpkgs than the host baseline
carries the same two risks ADR 0005 already named: ABI mismatch (an old package dropped into a
newer environment fails to build or collides), and a dependency on a third-party version index
(nixhub/lazamar) to map a version to a commit.

## Has historical-commit mixing proved insufficient?

Not yet, on the evidence we have. What we ship today (ADR 0005): resolve the version to a
historical nixpkgs commit, `import` that commit alongside the host baseline, and take the one
package from it. It is deterministic once locked (a 40-char rev is a complete pin, no sha
needed), it is offline at generate/check time, and `install --build` turns an ABI break into an
install-time refusal rather than a broken host. The pinned emit path now also has a byte-for-byte
golden (#25) and its own correctness bug is fixed (the emitter now parenthesises the mixed-in
import correctly). The known limits are inherent to mixing versions in Nix, not to this
particular mechanism, so there is no concrete failure driving a switch.

That answer is the headline: **do not implement #23 now.** The rest records what the
alternatives would actually buy us, so the decision is not relitigated from memory.

## Option 2: overrideAttrs (version + src + hash)

Override the package's `version` and `src` in an overlay:

```nix
htop.overrideAttrs (old: {
  version = "3.2.1";
  src = fetchFromGitHub { owner = "htop-dev"; repo = "htop"; rev = "3.2.1"; hash = "sha256-..."; };
})
```

What it gives: one nixpkgs rev for the whole host (no second fetch), and the pin lives in an
overlay rather than a mixed-in import.

Why it is not a general answer:

- It builds the *old source* against the *current* build inputs and dependency versions. That
  is the ABI-mismatch risk turned up to maximum: old releases routinely fail against newer
  autotools, compilers, or library majors, and `overrideAttrs` does nothing for the package's
  own dependencies (those stay at the baseline). Historical-commit mixing at least builds the
  package against the deps of its own era.
- It needs the source hash per version. That is a second resolution problem on top of
  version-to-commit, and it cannot be derived offline: someone or something has to prefetch it.
- It only reaches `src`/`version`. Anything the version change implies in the derivation (new
  build steps, patches, dependency bumps) is on the user. It is a scalpel for "same package,
  slightly different source," not for "give me the 2.x line."

Verdict: not a replacement. At most a narrow, opt-in escape hatch, and the project already has
one of those (`raw-nix`) for a user who wants to hand-write an `overrideAttrs` for a specific
package. No new machinery required to allow that.

## Option 3: flake input per package

Add one nixpkgs flake input per pinned version and pull the package from it:

```nix
inputs.nixpkgs_htop_321.url = "github:NixOS/nixpkgs/<commit>";
# ... environment.systemPackages = [ nixpkgs_htop_321.legacyPackages.${system}.htop ];
```

What it gives: the same substance as historical-commit mixing (a package from another rev),
expressed as a flake input instead of a `builtins.fetchGit` import.

Why it is not worth switching to:

- It is the flake-shaped version of what we already do, not a different capability. The version
  still resolves to a commit; the commit is still pinned; the package is still mixed in. The ABI
  story is identical.
- knixl deliberately does not target flakes (it emits plain NixOS modules, ADR 0001 scope). A
  flake-input approach would only make sense inside a project that is already a flake, and would
  restructure the emit model and the lock (flake.lock vs knixl.lock) to buy nothing over the
  current mechanism.

Verdict: no. It is a re-skin of the accepted approach for a project shape knixl does not target.

## When to revisit

Reopen #23 only against a concrete failure of historical-commit mixing, e.g.:

- A wanted version that *no single nixpkgs commit* ships (so there is no rev to mix in), where
  `overrideAttrs` on a nearby commit is the only route. That is the one case where option 2 wins,
  and it argues for `overrideAttrs` as a fallback resolver mode, not a replacement.
- Real, repeated cross-rev collisions in user configs that `--build` catches but users cannot
  work around, suggesting the whole mixing strategy needs rethinking.

Until one of those shows up, the current approach stands, and `KNIXL_PIN_RESOLVER` plus
`raw-nix` cover the escape hatches.

## Outcome

- Keep #23 open, labelled "revisit if insufficient", with this spike linked.
- No code, no ADR amendment (ADR 0005's deferral already documents the decision).
- If a fallback ever lands, the smallest sensible first step is an `overrideAttrs` resolver mode
  behind `KNIXL_PIN_RESOLVER`, scoped to the single "no commit ships this version" case, not a
  general second strategy.
