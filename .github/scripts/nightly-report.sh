#!/usr/bin/env bash
# Copies the latest nightly Proofs run's report into a folder, as `nightly-proofs-<date>.md`, and
# says in one line how the night went:
#
#   .github/scripts/nightly-report.sh "$HOME/Library/Application Support/Claude/side-session-notes/<lanes>"
#
# A second argument names the repository, `fennec-language/fennec` unless given.
#
# It needs `gh`, signed in. A night still running, or one with no report, is said so, and nothing
# is written. It exits 1 when the night failed, so whatever runs it can say so loudly.
set -eu
folder=$1
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
