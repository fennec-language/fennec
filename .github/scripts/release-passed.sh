#!/usr/bin/env bash
# Whether the release workflow's nightly run has nothing to do: a run of it on the same commit,
# other than a pull request's, already passed. Main often doesn't move overnight, and each run
# builds every platform twice. A run that failed, or none, leaves the night's run to build.
#
#   EVENT=schedule SHA=... GITHUB_REPOSITORY=owner/name .github/scripts/release-passed.sh
#
# It prints `true` or `false`, and why on standard error. Any other event prints `false` without
# asking, so a tag, a pull request or a run by hand always builds. It needs `gh`, with a token
# that reads the repository's runs.
set -euo pipefail
: "${EVENT:?}" "${SHA:?}" "${GITHUB_REPOSITORY:?}"
if [ "$EVENT" != schedule ]; then
  echo false
  exit 0
fi
passed=$(gh run list --repo "$GITHUB_REPOSITORY" --workflow release.yml --commit "$SHA" --status success \
  --json databaseId,event,url --jq '.[] | select(.event != "pull_request") | [(.databaseId | tostring), .event, .url] | join(" ")' | head -n 1)
if [ -n "$passed" ]; then
  read -r id event url <<< "$passed"
  echo "release run $id ($event) on $SHA passed, so tonight's has nothing to add: $url" >&2
  echo true
else
  echo "no release run on $SHA has passed, so tonight's builds it" >&2
  echo false
fi
