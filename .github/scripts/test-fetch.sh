#!/usr/bin/env bash
# Tests fetch.sh on a repository of its own with a second worktree: a fetch waits while another
# holds the lock, then fetches, and the lock is held for as long as git runs, so a second fetch
# from the other worktree waits for the first to finish.
#
# Nothing here is timed. Whatever holds the lock, or stands in for git, waits to be told to go on,
# so each check runs at a moment known by what has happened, however loaded the machine is: with
# two gates running, a fixed second once wasn't long enough for fetch.sh to reach its lock. Each
# wait gives up after two minutes, which only a broken fetch.sh takes.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
holder=""
fetching=""
trap '[ -z "$holder" ] || kill "$holder" 2> /dev/null || true; [ -z "$fetching" ] || kill "$fetching" 2> /dev/null || true; rm -rf "$work"' EXIT
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

# Waits for the file `$1` to be there, up to two minutes.
appears() {
  local tries=0
  until [ -e "$1" ]; do
    [ "$tries" -lt 1200 ] || return 1
    sleep 0.1
    tries=$((tries + 1))
  done
}

# Whether the lock can be taken now.
free() {
  perl -MFcntl=:flock -e 'open(my $held, ">>", shift) or die; exit(flock($held, LOCK_EX | LOCK_NB) ? 0 : 1)' "$lock"
}

# Another holds the lock until it's told to let it go: meanwhile the fetch waits, and once it's let
# go, the fetch fetches.
perl -MFcntl=:flock -e '
  my ($lock, $holding, $go) = @ARGV;
  open(my $held, ">>", $lock) or die;
  flock($held, LOCK_EX) or die;
  open(my $said, ">", $holding) or die;
  close($said);
  for (1 .. 1200) { last if -e $go; select(undef, undef, undef, 0.1) }
' "$lock" "$work/holding" "$work/go" &
holder=$!
appears "$work/holding" || { echo "not ok: the lock was never taken to test with" >&2; exit 1; }
(cd "$work/other" && "$here/fetch.sh" main) &
fetching=$!
# A fetch that didn't wait would be done well within this; one that waits is still there, however
# long it took to start. A fetch.sh that doesn't wait may pass on a slow enough machine, but one
# that does never fails here.
sleep 1
if ! kill -0 "$fetching" 2> /dev/null; then
  echo "not ok: the fetch didn't wait for the lock" >&2
  failed=1
fi
: > "$work/go"
wait "$holder"
holder=""
wait "$fetching" || { echo "not ok: the fetch failed once the lock was free" >&2; failed=1; }
fetching=""
git -C "$work/other" rev-parse --quiet --verify origin/main > /dev/null || { echo "not ok: nothing was fetched" >&2; failed=1; }

# The lock is held while git runs: a git that says when it's inside the fetch, and then waits to
# be told to finish, stands in for a slow fetch. While it's inside, the lock can't be taken.
mkdir "$work/bin"
cat > "$work/bin/git" << SLOW
#!/bin/sh
case "\$1" in
  fetch)
    : > "$work/inside"
    tries=0
    while [ ! -e "$work/finish" ] && [ "\$tries" -lt 1200 ]; do sleep 0.1; tries=\$((tries + 1)); done
    exit 0
    ;;
esac
exec $(command -v git) "\$@"
SLOW
chmod +x "$work/bin/git"
PATH="$work/bin:$PATH" "$here/fetch.sh" main &
fetching=$!
if ! appears "$work/inside"; then
  echo "not ok: git was never asked to fetch" >&2
  failed=1
elif free; then
  echo "not ok: the lock was free while git fetched" >&2
  failed=1
fi
: > "$work/finish"
wait "$fetching" || { echo "not ok: the slow fetch failed" >&2; failed=1; }
fetching=""
free || { echo "not ok: the lock was still held after git finished" >&2; failed=1; }
[ "$failed" -eq 0 ] && echo "ok: fetch.sh waits for the lock and holds it while git fetches"
exit "$failed"
