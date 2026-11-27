#!/usr/bin/env bash
# What `CI passed` in ci.yml decides: whether every job a run has to pass did, from the jobs'
# results `NEEDS` holds and, for the matrix jobs, from each one's own, which GitHub's API gives.
#
# A job GitHub never gave a runner, as happens when its macOS runners are short, is said plainly,
# apart from the code's failures, so nobody chases it as one: it was cancelled with no runner and
# no steps, and its check says "not acquired by Runner". It still fails the run, but for a macOS job
# on a pull request, which only warns, since the push to main runs it again.
#
#   NEEDS='{"lint":{"result":"success"},...}' EVENT=push RUN_ID=... RUN_ATTEMPT=1 \
#     GITHUB_REPOSITORY=owner/name .github/scripts/ci-passed.sh
#
# DOCS_ONLY, PARKED_CHANGED, PLATFORMS and COVERED are `changes`' outputs. It needs `gh`, with a token that
# reads the run's jobs and checks.
set -euo pipefail
: "${NEEDS:?}" "${EVENT:?}" "${RUN_ID:?}" "${RUN_ATTEMPT:?}" "${GITHUB_REPOSITORY:?}"
DOCS_ONLY=${DOCS_ONLY:-false}
PARKED_CHANGED=${PARKED_CHANGED:-false}
PLATFORMS=${PLATFORMS:-all}
COVERED=${COVERED:-false}
summary=${GITHUB_STEP_SUMMARY:-/dev/null}

# Every job of this attempt, as `id|name|conclusion|runner|steps`, split on `|` since tabs would
# run together where the runner is empty.
jobs=$(gh api --paginate "repos/$GITHUB_REPOSITORY/actions/runs/$RUN_ID/attempts/$RUN_ATTEMPT/jobs" \
  --jq '.jobs[] | [(.id | tostring), .name, (.conclusion // "unfinished"), (.runner_name // ""), (.steps | length | tostring)] | join("|")')

# Whether the job never got a runner.
# shellcheck source=.github/scripts/no-runner.sh
source "$(dirname "$0")/no-runner.sh"

# A macOS job of a pull request that never got a runner only warns.
macos='(macos-latest, [^)]*\)|x86-64 macOS \([^)]*\))$'
excused() {
  [ "$EVENT" = pull_request ] && [[ "$1" =~ $macos ]]
}

# The matrices' jobs, which the loop over `NEEDS` looks at one by one.
matrices='(^|/ )(Test|Native code on x86-64 macOS) \('


failed=0
waited=()
broke=()
no_runner() {
  echo "::error title=No runner::$1 never got a runner: GitHub had none to give it, so nothing in the code failed. Run it again with \`gh run rerun $RUN_ID --failed\`"
  waited+=("$1")
  failed=1
}

while read -r job result; do
  [ "$result" = success ] && continue
  [ "$result" = skipped ] && [ "$DOCS_ONLY" = true ] && continue
  # The macOS jobs alone, or every other: what the run leaves out isn't its to pass.
  if [ "$result" = skipped ] && [ "$PLATFORMS" = macos ] && [ "$job" != changes ] \
    && [ "$job" != test ] && [ "$job" != x86-mac ]; then
    continue
  fi
  [ "$result" = skipped ] && [ "$PLATFORMS" = others ] && [ "$job" = x86-mac ] && continue
  # A scheduled run whose commit a push run covers skips all but what's scheduled alone
  # (`covered-by-push.sh`).
  if [ "$result" = skipped ] && [ "$COVERED" = true ] && [ "$EVENT" = schedule ] \
    && [ "$job" != changes ] && [ "$job" != coverage ] && [ "$job" != audit ]; then
    continue
  fi
  # Coverage and the heap audit run only in the scheduled run and by hand.
  if [ "$result" = skipped ] && { [ "$job" = coverage ] || [ "$job" = audit ]; } \
    && [ "$EVENT" != schedule ] && [ "$EVENT" != workflow_dispatch ]; then
    continue
  fi
  if [ "$result" = skipped ] && [ "$EVENT" = merge_group ]; then
    [ "$job" = parked-sanitizers ] && [ "$PARKED_CHANGED" != true ] && continue
  fi
  # `test` has one result for its whole matrix, so each OS's job is looked at, and so is each
  # part of the Intel Mac's.
  case "$job" in
    test) pattern='(^|/ )Test \(' ;;
    x86-mac) pattern='(^|/ )Native code on x86-64 macOS \(' ;;
    *) pattern='' ;;
  esac
  if [ -n "$pattern" ] && [ "$result" != skipped ]; then
    looked_at=0
    while IFS='|' read -r id name conclusion runner steps; do
      [[ "$name" =~ $pattern ]] || continue
      looked_at=1
      [ "$conclusion" = success ] && continue
      if never_ran "$id" "$conclusion" "$runner" "$steps"; then
        if excused "$name"; then
          echo "::warning::$name never got a runner, so it doesn't count; the push to main runs it again"
        else
          no_runner "$name"
        fi
        continue
      fi
      echo "$name finished with: $conclusion"
      broke+=("$name: $conclusion")
      failed=1
    done <<< "$jobs"
    # Run through `staging.yml`, a job's name has that job's in front. If this ever found no jobs
    # at all, it can't say they passed.
    [ "$looked_at" = 1 ] || { echo "found no $job jobs to look at"; broke+=("$job: none found"); failed=1; }
    continue
  fi
  # Any other job that never got a runner is said so, by its name, below.
  echo "$job finished with: $result"
  [ "$result" = cancelled ] || broke+=("$job: $result")
  failed=1
done < <(jq -r 'to_entries[] | "\(.key) \(.value.result)"' <<< "$NEEDS")

# The jobs outside the matrices that never got a runner, which `NEEDS` names only by their keys,
# and any job cancelled for another reason.
if [ "$failed" = 1 ]; then
  while IFS='|' read -r id name conclusion runner steps; do
    [ "$conclusion" = cancelled ] || continue
    [[ "$name" =~ $matrices ]] && continue
    if never_ran "$id" "$conclusion" "$runner" "$steps"; then
      no_runner "$name"
    else
      broke+=("$name: cancelled")
    fi
  done <<< "$jobs"
fi

{
  if [ "${#waited[@]}" -gt 0 ]; then
    echo "### Never got a runner"
    echo
    echo "GitHub had no runner to give these, so nothing in the code failed. \`gh run rerun $RUN_ID --failed\` runs them again."
    echo
    printf -- '- %s\n' "${waited[@]}"
    echo
  fi
  if [ "${#broke[@]}" -gt 0 ]; then
    echo "### Failed"
    echo
    printf -- '- %s\n' "${broke[@]}"
  fi
} >> "$summary"
exit "$failed"
