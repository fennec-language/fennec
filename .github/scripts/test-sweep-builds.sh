#!/usr/bin/env bash
# Tries `sweep-builds.sh` on worktrees made for it: one with old and new build files, and one with a
# program running in it, which it must leave alone.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
sleeper=""
trap '[ -n "$sleeper" ] && kill "$sleeper" 2> /dev/null; rm -rf "$work"' EXIT
fail() {
  echo "test-sweep-builds: $1" >&2
  exit 1
}

trees="$work/worktrees"
mkdir -p "$trees/idle/target/debug/deps" "$trees/idle/target/debug/old" "$trees/busy/target/debug"
head -c 1000 /dev/zero > "$trees/idle/target/debug/deps/libold.rlib"
head -c 500 /dev/zero > "$trees/idle/target/debug/old/gone.o"
head -c 300 /dev/zero > "$trees/idle/target/debug/deps/libnew.rlib"
head -c 700 /dev/zero > "$trees/busy/target/debug/libold.rlib"
# Last read and written three days ago, so a sweep of two days takes them.
old="$(date -u -d '3 days ago' +%Y%m%d%H%M 2> /dev/null || date -u -v-3d +%Y%m%d%H%M)"
for file in idle/target/debug/deps/libold.rlib idle/target/debug/old/gone.o busy/target/debug/libold.rlib; do
  TZ=UTC touch -t "$old" "$trees/$file"
done
# Something running in the busy worktree.
(cd "$trees/busy" && exec sleep 60) &
sleeper=$!
# Not this shell's job any more, so its end isn't reported.
disown "$sleeper"
sleep 1

said="$("$here/sweep-builds.sh" --worktrees "$trees" --dry-run)"
echo "$said"
grep -qx "idle: would delete 2 files, 0.0 GB" <<< "$said" || fail "the dry run didn't count the two old files"
grep -qx "busy: skipped, something is running in it" <<< "$said" || fail "the busy worktree wasn't skipped"
[ -f "$trees/idle/target/debug/deps/libold.rlib" ] || fail "the dry run deleted something"

said="$("$here/sweep-builds.sh" --worktrees "$trees")"
echo "$said"
grep -qx "idle: deleted 2 files, 0.0 GB" <<< "$said" || fail "the sweep didn't say what it deleted"
[ ! -e "$trees/idle/target/debug/deps/libold.rlib" ] || fail "an old file is still there"
[ ! -e "$trees/idle/target/debug/old" ] || fail "the folder it emptied is still there"
[ -f "$trees/idle/target/debug/deps/libnew.rlib" ] || fail "a new file was deleted"
[ -f "$trees/busy/target/debug/libold.rlib" ] || fail "a file in the busy worktree was deleted"
grep -qx "freed 0.0 GB in all" <<< "$said" || fail "the sweep didn't say how much it freed"

# A file read in the two days is still in use, however old it was written.
head -c 100 /dev/zero > "$trees/idle/target/debug/read.d"
TZ=UTC touch -m -t "$old" "$trees/idle/target/debug/read.d"
said="$("$here/sweep-builds.sh" --worktrees "$trees")"
grep -qx "idle: nothing unused for 2 days" <<< "$said" || fail "a file read lately was taken: $said"

if "$here/sweep-builds.sh" --worktrees "$trees" --days two 2> /dev/null; then
  fail "--days two was taken"
fi
if "$here/sweep-builds.sh" --worktrees "$work/none" 2> /dev/null; then
  fail "a missing folder of worktrees was taken"
fi
echo "test-sweep-builds: passed"
