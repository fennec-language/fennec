#!/usr/bin/env bash
# Whether main's CI is green on every system, in one line, naming each job that failed in its
# newest finished run and the commit it has failed since:
#
#   .github/scripts/main-ci.sh [repository]
#
# says, for example,
#
#   main CI: red at 56a2af620 (run 43651800879): Test (windows-latest, llvm::) since 04975ac69
#
# and exits 1, so whatever runs it can say so loudly: a job red on one system, like Windows, is
# seen the day it goes red. A cancelled run says nothing, so the newest one that finished counts,
# and how far back a job has failed is looked for in the 30 finished runs before it: past them,
# it's "since at least". It needs `gh`, signed in. `nightly-report.sh` runs it.
set -euo pipefail
repository=${1:-fennec-language/fennec}
runs=$(gh run list --repo "$repository" --workflow CI --branch main --limit 200 \
  --json databaseId,conclusion,headSha --jq '.[] | select(.conclusion != "" and .conclusion != "cancelled") | "\(.databaseId) \(.headSha[0:9])"' | head -n 31)
if [ -z "$runs" ]; then
  echo "main CI: no finished run on main"
  exit 0
fi
read -r newest sha <<< "$(head -n 1 <<< "$runs")"
# The jobs that failed in a run, a name a line, leaving out the one that only sums the others up.
# Each run is asked once, however many jobs are looked for in it.
asked=$(mktemp -d)
trap 'rm -rf "$asked"' EXIT
failed_in() {
  [ -f "$asked/$1" ] || gh run view "$1" --repo "$repository" --json jobs \
    --jq '.jobs[] | select(.conclusion == "failure" or .conclusion == "timed_out") | select(.name != "CI passed") | .name' \
    > "$asked/$1"
  cat "$asked/$1"
}
failed=$(failed_in "$newest")
if [ -z "$failed" ]; then
  echo "main CI: green at $sha (run $newest)"
  exit 0
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
echo "main CI: red at $sha (run $newest): $said"
exit 1
