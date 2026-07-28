#!/usr/bin/env bash
# Report issues referenced by a release's commits that are still open.
#
# knixl references issues as `(#NN)` rather than closing them by keyword (one issue is often
# finished across several branches, so a keyword would close it on the first merge). Nothing
# closes automatically as a result, so this sweeps a commit range and lists every referenced
# issue still in the open state.
#
# Reads (all optional, so it runs locally as well as in Actions):
#   IN_FROM, IN_TO         explicit range ends; default to the previous tag and the pushed tag
#   GITHUB_REF_NAME        the pushed tag, when running from a tag push
#   GITHUB_REPOSITORY      owner/repo; defaults to the checkout's origin remote
#   GITHUB_STEP_SUMMARY    where the report goes; defaults to stdout
#
# Always exits 0: an open issue is something for a human to look at, not a release failure.
set -uo pipefail

repo="${GITHUB_REPOSITORY:-$(gh repo view --json nameWithOwner -q .nameWithOwner 2>/dev/null)}"
if [ -z "$repo" ]; then
  echo "cannot determine the repository (set GITHUB_REPOSITORY)" >&2
  exit 0
fi

to="${IN_TO:-}"
[ -z "$to" ] && to="${GITHUB_REF_NAME:-HEAD}"
from="${IN_FROM:-}"
if [ -z "$from" ]; then
  # The tag before `to`, walking back through its own ancestry.
  from="$(git describe --tags --abbrev=0 "${to}^" 2>/dev/null)" || from=""
fi

if [ -n "$from" ]; then
  range="${from}..${to}"
  range_label="\`${from}..${to}\`"
else
  # No earlier tag: the first release, so sweep everything reachable.
  range="$to"
  range_label="everything up to \`${to}\`"
fi

if ! git rev-parse "$to" >/dev/null 2>&1; then
  echo "unknown ref \`$to\`, nothing to sweep" >&2
  exit 0
fi

# Every `#NN` in the range's subjects and bodies, deduped and numerically sorted.
refs="$(git log --no-merges --pretty='%s%n%b' "$range" 2>/dev/null \
  | grep -oE '#[0-9]+' | tr -d '#' | sort -un)"

# Compose the report once, then emit it to the job summary (where a human reads it) and to
# stdout (so the run log carries it too: step summaries are not exposed over the REST API, and
# a report you cannot retrieve afterwards is not much of a report).
report_file="$(mktemp)"
trap 'rm -f "$report_file"' EXIT

emit() {
  if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    cat "$report_file" >>"$GITHUB_STEP_SUMMARY"
  fi
  cat "$report_file"
}

{
  echo "## Release issue sweep"
  echo
  echo "Range: ${range_label}"
  echo
} >>"$report_file"

if [ -z "$refs" ]; then
  echo "No issue references (\`#NN\`) in this range." >>"$report_file"
  emit
  exit 0
fi

open_count=0
checked=0
report=""

for n in $refs; do
  json="$(gh api "repos/${repo}/issues/${n}" 2>/dev/null)" || continue
  [ -z "$json" ] && continue
  checked=$((checked + 1))

  # Issues and pull requests share a numbering space; a PR carries a `pull_request` key. The
  # `(#NN)` in a squash-merge subject is usually the PR itself, so skip those.
  if [ "$(jq -r 'has("pull_request")' <<<"$json")" = "true" ]; then
    continue
  fi
  [ "$(jq -r '.state' <<<"$json")" != "open" ] && continue

  title="$(jq -r '.title' <<<"$json")"
  url="$(jq -r '.html_url' <<<"$json")"
  # Oldest commit in the range that mentions it: that is the substantive work, whereas the
  # newest is usually the release chore commit recapping it.
  commit="$(git log --oneline --no-merges --grep="#${n}\b" --reverse "$range" 2>/dev/null | head -1)"

  open_count=$((open_count + 1))
  report+="- [#${n}](${url}) ${title}"$'\n'
  [ -n "$commit" ] && report+="  - referenced by \`${commit}\`"$'\n'
  echo "::notice title=Issue #${n} still open::${title} (${url})"
done

if [ "$open_count" -eq 0 ]; then
  echo "Checked ${checked} referenced issue(s); none are still open. :white_check_mark:" \
    >>"$report_file"
  emit
  exit 0
fi

{
  echo "${open_count} referenced issue(s) shipped in this range are still open:"
  echo
  printf '%s' "$report"
  echo
  echo "Close whichever are genuinely done. knixl does not auto-close by keyword, because an"
  echo "issue is often finished across several branches."
} >>"$report_file"

emit
exit 0
