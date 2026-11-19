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
#   main CI macOS: green at 56a2af620 (run 43651800902)
#
# and exits 1 if either is red, so whatever runs it can say so loudly: a job red on one system,
# like Windows, is seen the day it goes red. A cancelled run says nothing, so the newest one that
# finished counts, and how far back a job has failed is looked for in the 30 finished runs before
# it: past them, it's "since at least". It needs `gh`, signed in. `nightly-report.sh` runs it.
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
red=0
says() {
  local workflow=$1
  local runs newest sha failed said job since back run at
  # The newest 31 that finished, picked in jq: cut off with `head`, `gh` was sometimes still
  # writing, died of the broken pipe, and took this script with it, printing nothing.
  runs=$(gh run list --repo "$repository" --workflow "$workflow" --branch main --limit 200 \
    --json databaseId,conclusion,headSha \
    --jq '[.[] | select(.conclusion != "" and .conclusion != "cancelled")] | .[:31][] | "\(.databaseId) \(.headSha[0:9])"')
  if [ -z "$runs" ]; then
    echo "main $workflow: no finished run on main"
    return 0
  fi
  read -r newest sha <<< "$(head -n 1 <<< "$runs")"
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
  echo "main $workflow: red at $sha (run $newest): $said"
  red=1
}

says CI
says "CI macOS"
exit "$red"
