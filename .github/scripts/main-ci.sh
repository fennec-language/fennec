#!/usr/bin/env bash
# Whether main's CI is green on every system, a line for each of its two workflows, `CI` and its
# macOS jobs' `CI macOS` (see `ci-macos.yml`), naming each job that failed in its newest finished
# run and the commit it has failed since:
#
#   .github/scripts/main-ci.sh [repository]
#
# says, for example,
#
#   main CI: red at 56a2af620 (run 43651800879): Test (windows-latest, llvm::) since 04975ac69
#   main CI macOS: red at 9914acc08 (run 44951580323, still going): ci / Test (macos-latest, fennec) since 9914acc08
#
# and exits 1 if either is red, so whatever runs it can say so loudly: a job red on one system,
# like Windows, is seen the day it goes red. A cancelled run says nothing, so the newest one that
# finished counts, unless a run newer than it, still going, has a job that failed already: that
# one counts, so a job's red is seen when it ends, not when the run's last job does, which on
# 2026-11-21 was 25 minutes later. Newest is by the commit's place in main's history, not when its
# run was made: a run of a commit can be made after its child's, and on 2026-11-21 this said red at
# the commit before a fix, whose run was made 4 minutes after the fix's green one. How far back a job has failed is looked for in the 30 finished
# runs before the one that counts: past them, it's "since at least". It needs `gh`, signed in.
# `nightly-report.sh` runs it.
set -euo pipefail
repository=${1:-fennec-language/fennec}
# The jobs that failed in a run, a name a line, leaving out the one that only sums the others up,
# which a run of `CI macOS` names `ci / CI passed`. Each run is asked once, however many jobs are
# looked for in it.
asked=$(mktemp -d)
trap 'rm -rf "$asked"' EXIT
failed_in() {
  [ -f "$asked/$1" ] || gh run view "$1" --repo "$repository" --json jobs \
    --jq '.jobs[] | select(.conclusion == "failure" or .conclusion == "timed_out") | select(.name | test("(^|/ )CI passed$") | not) | .name' \
    > "$asked/$1"
  cat "$asked/$1"
}

# The line for one workflow, by its name, setting `red` if it is. It never returns a failure, so
# `set -e` still stops this at a `gh` that fails, rather than taking that for green.
# Main's newest 300 commits, newest first, a short hash a line. Main is squashed into, one parent
# each, so GitHub's order is the first-parent one.
history=$(for page in 1 2 3; do
  gh api "repos/$repository/commits?sha=main&per_page=100&page=$page" --jq '.[].sha[0:9]'
done)

red=0
says() {
  local workflow=$1
  local listed runs going still newest sha failed said job since back run at state
  # The runs going and the ones that finished, newest first, picked in jq: cut off with `head`,
  # `gh` was sometimes still writing, died of the broken pipe, and took this script with it,
  # printing nothing. One waiting for a runner has no job yet, so it says nothing.
  listed=$(gh run list --repo "$repository" --workflow "$workflow" --branch main --limit 200 \
    --json databaseId,status,conclusion,headSha \
    --jq '[.[] | select(.status == "in_progress" or (.conclusion != "" and .conclusion != "cancelled"))] | .[:40][] | "\(.databaseId) \(.headSha[0:9]) \(.status)"')
  # Newest commit first, and for one commit's runs, newest made first. A run of a commit past the
  # history read comes after every one in it, in the order GitHub listed it.
  listed=$(awk 'NR == FNR { if (!($1 in at)) at[$1] = FNR; next }
    { print ($2 in at ? at[$2] : 100000 + FNR), $0 }' <(printf '%s\n' "$history") - <<< "$listed" \
    | sort -s -n -k1,1 | cut -d' ' -f2-)
  # The newest 31 that finished.
  runs=$(awk '$3 == "completed" { print $1, $2; if (++n == 31) exit }' <<< "$listed")
  # The newest run going, newer than every one that finished, with a job that failed already.
  going=""
  while read -r run at state; do
    [ -n "$run" ] && [ "$state" != completed ] || break
    if [ -n "$(failed_in "$run")" ]; then
      going="$run $at"
      break
    fi
  done <<< "$listed"
  still=""
  if [ -n "$going" ]; then
    read -r newest sha <<< "$going"
    still=", still going"
  elif [ -z "$runs" ]; then
    echo "main $workflow: no finished run on main"
    return 0
  else
    read -r newest sha <<< "$(head -n 1 <<< "$runs")"
  fi
  failed=$(failed_in "$newest")
  if [ -z "$failed" ]; then
    echo "main $workflow: green at $sha (run $newest)"
    return 0
  fi
  said=""
  while IFS= read -r job; do
    # The oldest commit in an unbroken line of runs this job failed in.
    since=$sha
    back="at least "
    while read -r run at; do
      [ "$run" = "$newest" ] && continue
      if failed_in "$run" | grep -qxF "$job"; then
        since=$at
      else
        back=""
        break
      fi
    done <<< "$runs"
    said="${said:+$said; }$job since $back$since"
  done <<< "$failed"
  echo "main $workflow: red at $sha (run $newest$still): $said"
  red=1
}

says CI
says "CI macOS"
exit "$red"
