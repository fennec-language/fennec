#!/usr/bin/env bash
# Copies the latest nightly Proofs run's report into a folder, as `nightly-proofs-<date>.md`, and
# says in one line how the night went:
#
#   .github/scripts/nightly-report.sh "$HOME/Library/Application Support/Claude/side-session-notes/<lanes>"
#
# A second argument names the repository, `fennec-language/fennec` unless given.
#
# It also copies the night's mutants no test caught, ranked by file, as `mutants-<date>.md`, and
# this Mac's flake ledger (see `flakes.sh`), listed by test with `fennec-flakes` and
# each entry whole under it, as `flakes-<date>.md`, so every flake the gates saw is there to read.
#
# It says too whether main's CI is green on every system, naming each job that isn't, with
# `main-ci.sh`, and warns when GitHub holds more than 25 branches.
#
# It needs `gh`, signed in. A night still running, or one with no report, is said so, and nothing
# is written. It exits 1 when the night failed, or a job of main's CI is red on any system, so
# whatever runs it can say so loudly.
set -eu
folder=$1
here=$(cd "$(dirname "$0")" && pwd)
ledger="${FENNEC_HOME:-$HOME/.fennec}/flakes.log"
today=$(date -u +%Y-%m-%d)
if [ -f "$ledger" ]; then
  {
    echo "# Flakes the gates on this Mac saw, $today"
    echo
    echo '```'
    "$here/fennec-flakes" "$ledger"
    echo '```'
    echo
    echo "## Every entry"
    echo
    echo '```'
    cat "$ledger"
    echo '```'
  } > "$folder/flakes-$today.md"
  echo "flakes: $(grep -c '^=== flake$' "$ledger") in the ledger, listed in $folder/flakes-$today.md"
else
  echo "flakes: none, as there's no ledger at $ledger"
fi
repository=${2:-fennec-language/fennec}
# Main's CI on every system, in a line: a job red on one, like Windows, is said here, and fails the
# report, the day it goes red.
main_ci=0
"$here/main-ci.sh" "$repository" || main_ci=1
# The weekly sweep (`sweep-branches.yml`) deletes the branches nothing needs, so past 25 something
# is keeping them, or it isn't running: a warning, not a failed night.
branches=$(gh api --paginate "repos/$repository/branches?per_page=100" --jq 'length' 2> /dev/null \
  | awk '{ total += $1 } END { print total + 0 }')
if [ "$branches" -gt 25 ]; then
  echo "warning: $repository has $branches branches, more than 25: https://github.com/$repository/branches"
fi
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
# The night's mutants, from the latest scheduled Mutation tests run, as `mutants-<date>.md`: each
# crate's slice, with what no test caught ranked by file. Survivors are reported, never a failed night.
read -r mutants finished started <<< "$(gh run list --repo "$repository" --workflow mutants.yml --event schedule \
  --limit 1 --json databaseId,status,createdAt --jq '.[0] | "\(.databaseId) \(.status) \(.createdAt)"')"
if [ -z "${mutants:-}" ]; then
  echo "mutants: no nightly run yet"
elif [ "$finished" != completed ]; then
  echo "mutants: the nightly run $mutants is still going"
elif gh run download "$mutants" --repo "$repository" --pattern 'mutants-nightly-*' --dir "$work/mutants" > /dev/null 2>&1; then
  night=${started%%T*}
  {
    echo "# Mutants no test caught, $night"
    echo
    echo "https://github.com/$repository/actions/runs/$mutants"
    # One artifact comes out in the folder itself, several in a folder each.
    for ranked in "$work"/mutants/ranked.md "$work"/mutants/*/ranked.md; do
      [ -f "$ranked" ] || continue
      echo
      cat "$ranked"
    done
  } > "$folder/mutants-$night.md"
  missed=$(cat "$work"/mutants/mutants.out/missed.txt "$work"/mutants/*/mutants.out/missed.txt 2> /dev/null | grep -c . || true)
  echo "mutants on $night: $missed no test caught, ranked by file in $folder/mutants-$night.md"
else
  echo "mutants: the nightly run $mutants kept nothing to read"
fi
read -r run conclusion created <<< "$(gh run list --repo "$repository" --workflow proofs.yml --limit 1 \
  --json databaseId,conclusion,createdAt --jq '.[0] | "\(.databaseId) \(.conclusion // "running") \(.createdAt)"')"
if [ -z "${run:-}" ]; then
  echo "no Proofs run yet"
  exit "$main_ci"
fi
if [ "$conclusion" = running ] || [ "$conclusion" = "" ]; then
  echo "the Proofs run $run is still going"
  exit "$main_ci"
fi
if ! gh run download "$run" --repo "$repository" --name nightly-report --dir "$work" > /dev/null 2>&1; then
  echo "the Proofs run $run ($conclusion) kept no report"
  exit "$main_ci"
fi
day=${created%%T*}
cp "$work/nightly.md" "$folder/nightly-proofs-$day.md"
# The soaks' curves from the same night, after the rest: each real program's memory and live
# blocks over an hour, where they started and ended and how fast they rose.
if gh run download "$run" --repo "$repository" --pattern 'soak-*' --dir "$work/soak" > /dev/null 2>&1; then
  cat "$work"/soak/soak.md "$work"/soak/*/soak.md 2> /dev/null >> "$folder/nightly-proofs-$day.md" || true
fi
# The big codebase's steps at each size, and how each grew against the program.
if gh run download "$run" --repo "$repository" --name big-codebase --dir "$work/big" > /dev/null 2>&1; then
  cat "$work/big/big-codebase.md" 2> /dev/null >> "$folder/nightly-proofs-$day.md" || true
fi
# The tests that failed, by name, so a failed night reads in one line.
failed=$(sed -n 's/^Failed tests: //p' "$work/nightly.md" | head -n 1)
echo "Proofs on $day: $conclusion${failed:+, failed tests: $failed}, report in $folder/nightly-proofs-$day.md"
[ "$conclusion" = success ] && [ "$main_ci" -eq 0 ]
