# Release changelog: prompt and template

`CHANGELOG.md` at the repo root is the source of the release notes. cargo-dist
reads it at tag time and puts the matching version's section into the GitHub
release body (see `release.yml`, "title/body based on your changelogs"). So the
notes are only as good as the section you write before you tag.

Each release, turn the `[Unreleased]` section into a dated version section by
running the prompt below over the commit range since the last tag. Do it before
the `chore(release): x.y.z` version bump, so the entry rides in on the release
PR. cargo-dist matches the section heading to the version being released, so the
heading has to read `## [x.y.z] - YYYY-MM-DD`.

## The prompt

Feed this to the release-runner (a person or an agent). It assumes you are at the
repo root with the tags fetched.

> Write the `CHANGELOG.md` entry for the release `<x.y.z>`.
>
> Inputs, gather them first:
> - `git log --no-merges --pretty="%s%n%b" v<prev>..HEAD` for the merged work.
> - `gh pr list --state merged --search "merged:>=<date-of-prev-tag>"` and the
>   issues those PRs closed, for the user-facing framing and the numbers to link.
> - The ADRs added in the range (`git diff --stat v<prev>..HEAD -- docs/adr`), so
>   an architectural change points at its ADR.
>
> Then write one section, following the template below:
> - Group by `Added` / `Changed` / `Fixed` (drop any group with nothing in it).
>   `Added` is new capability, `Changed` is a change to existing behaviour or
>   output, `Fixed` is a bug. A version bump commit and pure-internal churn (CI,
>   refactors with no output change) do not get an entry.
> - Write for someone using knixl, not for someone reading the diff: name the KDL
>   they would write and the option or output it produces, not the Rust symbol.
> - Link the PR or issue number in parentheses at the end of the bullet, e.g.
>   `(#75)`. Point at the ADR when there is one, e.g. `(#64, ADR 0011)`.
> - Call out any change that alters generated output for an existing project, and
>   say plainly that `knixl upgrade` will show it as a regeneration. The
>   `networking.hostName` change in 1.1.0 is the model for this.
> - Match the writing voice (invoke the writing-voice skill): British spelling,
>   no em-dashes or en-dashes, none of the banned AI-tell vocabulary, and no
>   "X, not Y" contrasts. Bullets stay terse; the one-line summary under the
>   heading leads with what the release is about.
>
> Put the new section directly under `## [Unreleased]`, leave `[Unreleased]`
> empty, and add the two compare links at the bottom of the file (`[Unreleased]`
> now compares the new tag to HEAD, and a fresh `[x.y.z]` compares it to the
> previous tag).

## The template

```markdown
## [x.y.z] - YYYY-MM-DD

One line on what this release is about.

### Added
- New capability, in terms of the KDL a user writes and what it emits (#NN).

### Changed
- A change to existing behaviour or output; note if it forces a regeneration (#NN).

### Fixed
- A bug, described by what went wrong before (#NN).
```

Drop the empty groups. A release with only fixes has only a `Fixed` group.

## Worked shape

The 1.1.0 and 1.2.0 sections in `CHANGELOG.md` are the reference. 1.1.0 is the
busy case (all three groups, an ADR link, and the `networking.hostName`
regeneration callout under `Changed`); 1.2.0 is the small case (one feature, its
generalisation under `Changed`, and one `Fixed`).
