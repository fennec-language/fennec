#!/usr/bin/env bash
# Whether CI's scheduled run has nothing to add on its commit: a push run of `ci.yml` on the same
# commit is waiting, running or passed. Then the scheduled run's jobs that a push runs too are
# skipped, and only what's scheduled alone runs, coverage and the heap audit, so the two runs
# don't compete for runners. On 2026-11-26 a scheduled run on main's 661ec0c3a started beside
# the push run on it, still going, and their 28 jobs held main up for over an hour. A push run
# that failed or was cancelled leaves the scheduled run whole, to check again.
#
#   EVENT=schedule SHA=... GITHUB_REPOSITORY=owner/name .github/scripts/covered-by-push.sh
#
# It prints `true` or `false`, and why on standard error. Any other event prints `false` without
# asking. It needs `gh`, with a token that reads the repository's runs.
set -euo pipefail
: "${EVENT:?}" "${SHA:?}" "${GITHUB_REPOSITORY:?}"
if [ "$EVENT" != schedule ]; then
  echo false
  exit 0
fi
runs=$(gh run list --repo "$GITHUB_REPOSITORY" --workflow ci.yml --event push --commit "$SHA" \
  --json databaseId,status,conclusion,url --jq '.[] | [(.databaseId | tostring), .status, .conclusion, .url] | join(" ")')
while read -r id status conclusion url; do
  [ -n "$id" ] || continue
  if [ "$status" != completed ]; then
    echo "push run $id on $SHA is $status, so this scheduled run leaves its jobs to it: $url" >&2
    echo true
    exit 0
  fi
  if [ "$conclusion" = success ]; then
    echo "push run $id on $SHA passed, so this scheduled run has nothing to add but what's scheduled alone: $url" >&2
    echo true
    exit 0
  fi
done <<< "$runs"
echo "no push run on $SHA is waiting, running or passed, so this scheduled run runs every job" >&2
echo false
