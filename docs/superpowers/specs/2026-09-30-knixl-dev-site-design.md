# knixl.dev site: design

Status: agreed in conversation 2026-09-30; building.

A marketing and documentation site for knixl at `knixl.dev`, aimed at Nix users deciding
whether to try it. GitHub stays the home for issues and contributions.

## Content

- **Landing (`/`)**: hero (logo, the README one-liner, a copyable install command, links to
  GitHub and the docs), a looping CLI video, and the `web` example's KDL beside the Nix it
  generates. Then feature run-throughs, each a heading, two or three sentences and a video or a
  code pair: generate and check; lockfile and drift (Stale versus Drifted); oracle validation;
  version pinning; the module system and stdlib; the TUI; the whole system flake (ADR 0014). A
  "how it works" strip (KDL, modules, IR, formatter, lock, from docs/01), install options
  (`cargo install`, attested binaries, mise), footer.
- **Docs (`/docs/...`)**: `docs/00`-`06` as the guide and the ADRs as their own sidebar group,
  searchable.
- **Examples (`/examples/<host>/`)**: each `examples/hosts/*.kdl` with its generated `.nix` and
  side-files.
- **Changelog (`/changelog/`)**: `CHANGELOG.md`.

Not in scope: blog, an in-browser playground, versioned docs, analytics.

## Build

- Astro with Starlight in `site/`, pnpm, Node 24. Node and pnpm are pinned per mise task
  (`mise run site:dev`, `site:build`), so a plain `mise install` in the repo does not fetch them.
- `site/scripts/sync-content.mjs` runs before every dev and build. It generates, into
  `src/content/docs/` and gitignored:
  - a page per doc and ADR: the first `# heading` becomes the frontmatter title and is removed,
    the filename number sets the sidebar order;
  - links rewritten: another doc's `.md` goes to its site route, a repo path (`crates/...`,
    `examples/...`, `CHANGELOG.md`) goes to that file on GitHub at `main`;
  - a page per example host pairing `examples/hosts/<h>.kdl` with `examples/expected/<h>*.nix`
    (the golden outputs, so the site shows knixl's real output);
  - the changelog page.
- The landing page is a committed `src/content/docs/index.mdx` on Starlight's `splash` template
  with custom components, so the header, search and theme toggle match the docs. It reads the
  `web` pair and the version (from `Cargo.toml`) at build time.
- `starlight-links-validator` fails the build on a broken internal link.
- Colours come from the logo gradient (pink `#ff6ab8`, violet `#a78bfa`, teal `#5ee6c8`); no CSS
  framework.

## Media

- One VHS `.tape` per video in `site/media/tapes/`, run by `site/media/record.sh` against a
  throwaway copy of `examples/` with the real `knixl` binary, so recording never touches the
  repo. VHS and ttyd are pinned on the `site:record` task.
- Recordings: generate/check/doc; drift (Stale, then Drifted and the refusal); the oracle
  refusing a misspelt option; a pinned `install`; a TUI tour; the flake-input
  upgrade/generate/check. Anything that would need the network uses `rev=` pins or a stub
  resolver.
- Each renders WebM and MP4 plus a poster PNG into `site/public/media/`, committed. Target under
  1.5 MB each. CI does not record. `docs/release-changelog.md` gains a step: re-record when a
  release changes CLI output.
- On the page: muted, looping, `playsinline`, lazy, with the poster until loaded; under
  `prefers-reduced-motion` no autoplay, a play control instead. Each has a caption and the
  commands as copyable text.

## Deploy

- `.github/workflows/site.yml`. Pull requests touching `site/`, `docs/`, `examples/`,
  `CHANGELOG.md` or `Cargo.toml` build only (`pnpm install --frozen-lockfile`, `astro check`,
  `astro build`). Pushes to main on those paths, and manual dispatch, build and force-push the
  output as a single commit to an orphan `gh-pages` branch with plain `git` and the workflow's
  `GITHUB_TOKEN` (`contents: write` on that job only), including `CNAME` (`knixl.dev`) and
  `.nojekyll`. Superseded deploys are cancelled.
- One-time: Pages source set to `gh-pages`, custom domain `knixl.dev`, HTTPS enforced once the
  certificate is issued. DNS (apex A/AAAA to GitHub Pages, `www` CNAME) is already in place.

## Testing

CI builds on every relevant PR. Before the first deploy the built site is previewed locally and
checked in a browser at desktop and phone widths: hero, videos (and reduced motion), code panels
in both themes, search, and a sample of doc links.
