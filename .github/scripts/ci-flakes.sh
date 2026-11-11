#!/usr/bin/env bash
# CI's part of the flake ledger: run when every job of a rerun passed, it finds the jobs that
# failed in the run's earlier attempts, on this same commit, and writes each failure in them to
# `flakes.log` as a flake, with what it printed, for the run to keep as an artifact:
#
#   GH_TOKEN=... REPO=owner/name RUN_ID=... RUN_ATTEMPT=2 SHA=... .github/scripts/ci-flakes.sh
#
# A job's log is GitHub's, with a time in front of each line, which is taken off. A test job that
# fails says the runner's load first, which goes in the entry. See `flakes.sh` for the ledger.
set -u
here="$(cd "$(dirname "$0")" && pwd)"
: > flakes.log
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
for ((attempt = 1; attempt < RUN_ATTEMPT; attempt++)); do
  gh api --paginate "repos/$REPO/actions/runs/$RUN_ID/attempts/$attempt/jobs" \
    -q '.jobs[] | select(.conclusion == "failure") | [.id, .name, .completed_at] | @tsv' > "$work/failed" || exit 1
  while IFS="$(printf '\t')" read -r id name completed; do
    gh api "repos/$REPO/actions/jobs/$id/logs" > "$work/raw" || continue
    sed -E 's/^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9:.]+Z ?//' "$work/raw" > "$work/log"
    load=$(sed -n 's/^load when it failed: //p' "$work/log" | head -n 1)
    LOAD=${load:-unknown} TREE="the tree of $SHA" \
      "$here/flakes.sh" entries "CI: $name (attempt $attempt)" "$SHA" "$completed" "$work/log" >> flakes.log
  done < "$work/failed"
done
if [ -s flakes.log ]; then
  echo "::warning::this run passed on attempt $RUN_ATTEMPT after failing before on the same commit: each failure is in the flakes artifact"
  "$here/fennec-flakes" flakes.log
fi
