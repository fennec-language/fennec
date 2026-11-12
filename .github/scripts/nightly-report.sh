#!/usr/bin/env bash
# Copies the latest nightly Proofs run's report into a folder, as `nightly-proofs-<date>.md`, and
# says in one line how the night went:
#
#   .github/scripts/nightly-report.sh "$HOME/Library/Application Support/Claude/side-session-notes/<lanes>"
#
# A second argument names the repository, `fennec-language/fennec` unless given.
#
# It also copies this Mac's flake ledger (see `flakes.sh`), listed by test with `fennec-flakes` and
# each entry whole under it, as `flakes-<date>.md`, so every flake the gates saw is there to read.
#
# It needs `gh`, signed in. A night still running, or one with no report, is said so, and nothing
# is written. It exits 1 when the night failed, so whatever runs it can say so loudly.
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
read -r run conclusion created <<< "$(gh run list --repo "$repository" --workflow proofs.yml --limit 1 \
  --json databaseId,conclusion,createdAt --jq '.[0] | "\(.databaseId) \(.conclusion // "running") \(.createdAt)"')"
if [ -z "${run:-}" ]; then
  echo "no Proofs run yet"
  exit 0
fi
if [ "$conclusion" = running ] || [ "$conclusion" = "" ]; then
  echo "the Proofs run $run is still going"
  exit 0
fi
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
if ! gh run download "$run" --repo "$repository" --name nightly-report --dir "$work" > /dev/null 2>&1; then
  echo "the Proofs run $run ($conclusion) kept no report"
  exit 0
fi
day=${created%%T*}
cp "$work/nightly.md" "$folder/nightly-proofs-$day.md"
echo "Proofs on $day: $conclusion, report in $folder/nightly-proofs-$day.md"
[ "$conclusion" = success ]
