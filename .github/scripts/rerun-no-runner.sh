#!/usr/bin/env bash
# Runs a failed run's jobs again when every job that failed did so only because GitHub never gave
# it a runner, as `no-runner.sh` tells, and the run hasn't been run three times already. A run with
# any other failure is left as it is, and so is the summary job, `CI passed`, which fails because
# the others did and runs again with them.
#
#   RUN_ID=... RUN_ATTEMPT=1 GITHUB_REPOSITORY=owner/name .github/scripts/rerun-no-runner.sh
#
# `rerun-no-runner.yml` runs it when a `CI macOS` run on main fails. It needs `gh`, with a token
# that reads the run's jobs and checks and runs jobs again.
set -euo pipefail
: "${RUN_ID:?}" "${RUN_ATTEMPT:?}" "${GITHUB_REPOSITORY:?}"
# The first attempt and two more.
attempts=3

# shellcheck source=.github/scripts/no-runner.sh
source "$(dirname "$0")/no-runner.sh"

if [ "$RUN_ATTEMPT" -ge "$attempts" ]; then
  echo "run $RUN_ID has run $RUN_ATTEMPT times, so it isn't run again"
  exit 0
fi

waited=()
failed=()
while IFS='|' read -r id name conclusion runner steps; do
  case "$conclusion" in success | skipped | neutral) continue ;; esac
  [[ "$name" =~ (^|/\ )CI\ passed$ ]] && continue
  if never_ran "$id" "$conclusion" "$runner" "$steps"; then
    waited+=("$name")
  else
    failed+=("$name: $conclusion")
  fi
done < <(gh api --paginate "repos/$GITHUB_REPOSITORY/actions/runs/$RUN_ID/attempts/$RUN_ATTEMPT/jobs" \
  --jq '.jobs[] | [(.id | tostring), .name, (.conclusion // "unfinished"), (.runner_name // ""), (.steps | length | tostring)] | join("|")')

if [ "${#failed[@]}" -gt 0 ]; then
  echo "run $RUN_ID failed for more than want of a runner, so it isn't run again:"
  printf '  %s\n' "${failed[@]}"
  exit 0
fi
if [ "${#waited[@]}" = 0 ]; then
  echo "run $RUN_ID has no job that never got a runner, so it isn't run again"
  exit 0
fi
echo "run $RUN_ID, attempt $RUN_ATTEMPT: these never got a runner, so they run again:"
printf '  %s\n' "${waited[@]}"
gh run rerun "$RUN_ID" --failed --repo "$GITHUB_REPOSITORY"
