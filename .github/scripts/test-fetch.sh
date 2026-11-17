#!/usr/bin/env bash
# Tests fetch.sh on a repository of its own with a second worktree: a fetch waits while another
# holds the lock, then fetches, and the lock is held for as long as git runs, so a second fetch
# from the other worktree waits for the first to finish.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
holder=""
trap '[ -z "$holder" ] || kill "$holder" 2> /dev/null || true; rm -rf "$work"' EXIT
export GIT_AUTHOR_NAME=test GIT_AUTHOR_EMAIL=test@example.com GIT_COMMITTER_NAME=test GIT_COMMITTER_EMAIL=test@example.com
git init --quiet --bare --initial-branch=main "$work/origin.git"
git clone --quiet "$work/origin.git" "$work/checkout" 2> /dev/null
cd "$work/checkout"
echo one > file
git add file
git commit --quiet -m one
git push --quiet origin HEAD:main
git worktree add --quiet -b other "$work/other" main
lock="$(git rev-parse --path-format=absolute --git-common-dir)/fennec-fetch.lock"
failed=0

# Another holds the lock for two seconds: the fetch waits, then fetches.
perl -MFcntl=:flock -e 'open(my $held, ">>", shift) or die; flock($held, LOCK_EX) or die; print "held\n"; $| = 1; sleep 2' "$lock" > "$work/held" &
holder=$!
until grep -q held "$work/held" 2> /dev/null; do sleep 0.1; done
started=$(date +%s)
(cd "$work/other" && "$here/fetch.sh" main)
waited=$(($(date +%s) - started))
wait "$holder"
holder=""
if [ "$waited" -lt 1 ]; then
  echo "not ok: the fetch didn't wait for the lock (took ${waited} s)" >&2
  failed=1
fi
git -C "$work/other" rev-parse --quiet --verify origin/main > /dev/null || { echo "not ok: nothing was fetched" >&2; failed=1; }

# The lock is held while git runs: a git that sleeps stands in for a slow fetch, and while it
# runs, the lock can't be taken.
mkdir "$work/bin"
cat > "$work/bin/git" << SLOW
#!/bin/sh
case "\$1" in fetch) sleep 2; exit 0 ;; esac
exec $(command -v git) "\$@"
SLOW
chmod +x "$work/bin/git"
PATH="$work/bin:$PATH" "$here/fetch.sh" main &
fetching=$!
sleep 1
if perl -MFcntl=:flock -e 'open(my $held, ">>", shift) or die; exit(flock($held, LOCK_EX | LOCK_NB) ? 0 : 1)' "$lock"; then
  echo "not ok: the lock was free while git fetched" >&2
  failed=1
fi
wait "$fetching" || { echo "not ok: the slow fetch failed" >&2; failed=1; }
[ "$failed" -eq 0 ] && echo "ok: fetch.sh waits for the lock and holds it while git fetches"
exit "$failed"
