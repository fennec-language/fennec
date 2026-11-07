#!/bin/bash
# Deletes the caches nothing will restore again, so the repository stays under GitHub's 10 GB.
# Over it, GitHub drops the least recently used caches, and a job whose cache is dropped builds
# everything again: Windows' tests took 10 minutes more that way.
#
# A kind of cache is its key with the parts that change from run to run taken out: commit hashes,
# Cargo.lock's and the toolchain's hashes, run numbers. On main, the two of each kind used last
# are kept, so a job restoring one as another saves never loses it. A pull request's caches go
# once nothing has used them for two days.
#
#   prune-caches.sh            deletes them, with GH_TOKEN allowed to write the repository's actions
#   prune-caches.sh --dry-run  lists what it would delete
set -euo pipefail

repo="$GITHUB_REPOSITORY"
dry_run=false
[ "${1:-}" = --dry-run ] && dry_run=true

listed="$(mktemp)"
trap 'rm -f "$listed"' EXIT
gh api --paginate "repos/$repo/actions/caches?per_page=100&sort=last_accessed_at&direction=desc" \
  --jq '.actions_caches[] | [.id, .ref, .key, .last_accessed_at, .size_in_bytes] | @tsv' > "$listed"
# GNU's date, as on the runner, or BSD's, as on a Mac.
cutoff="$(date -u -d '2 days ago' +%Y-%m-%dT%H:%M:%SZ 2> /dev/null || date -u -v-2d +%Y-%m-%dT%H:%M:%SZ)"

# Most recently used first, so the first two of each kind are the ones kept.
awk -F'\t' -v cutoff="$cutoff" '
  {
    kind = $3
    # Eight hex digits or more, written out: the awk on Ubuntu is mawk, which may not take {8,}.
    gsub(/-[0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f]+/, "", kind)
    gsub(/(-[0-9]+)+$/, "", kind)
    if ($2 == "refs/heads/main") {
      if (++seen[kind] > 2) print $1 "\t" $5 "\t" $3
    } else if ($4 < cutoff) {
      print $1 "\t" $5 "\t" $3
    }
  }
' "$listed" | {
  count=0
  bytes=0
  while IFS=$'\t' read -r id size key; do
    count=$((count + 1))
    bytes=$((bytes + size))
    if [ "$dry_run" = true ]; then
      echo "would delete $key"
    else
      gh api -X DELETE "repos/$repo/actions/caches/$id" > /dev/null || echo "couldn't delete $key"
    fi
  done
  echo "$count caches, $((bytes / 1048576)) MB, $([ "$dry_run" = true ] && echo "to delete" || echo deleted)"
}
