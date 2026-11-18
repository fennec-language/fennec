#!/usr/bin/env bash
# Tries `sweep-builds.sh` on worktrees made for it: one nothing has built in for three days, whose
# `target/` it deletes; one built in today, which it leaves whole, though most of its files are old;
# one with a program running in it, which it leaves alone; a `-keep` worktree and a session's own,
# as old as the first, which it leaves alone too; and one built in three hours ago, which only a
# sweep by hours takes.
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
mkdir -p "$trees/idle/target/debug/deps" "$trees/idle/target/debug/old" "$trees/active/target/debug/deps" \
  "$trees/busy/target/debug" "$trees/baseline-keep/target/release" "$trees/quiet-turing-a1b2c3/target/debug" \
  "$trees/hours/target/debug"
head -c 1000 /dev/zero > "$trees/idle/target/debug/deps/libold.rlib"
head -c 500 /dev/zero > "$trees/idle/target/debug/old/gone.o"
head -c 800 /dev/zero > "$trees/active/target/debug/deps/libstable.rlib"
head -c 300 /dev/zero > "$trees/active/target/debug/deps/libnew.rlib"
head -c 700 /dev/zero > "$trees/busy/target/debug/libold.rlib"
head -c 600 /dev/zero > "$trees/baseline-keep/target/release/fennec"
head -c 400 /dev/zero > "$trees/quiet-turing-a1b2c3/target/debug/fennec"
head -c 200 /dev/zero > "$trees/hours/target/debug/fennec"
# Written three days ago, so a sweep of two days takes them. Only the active worktree's newest
# file is from today: its other library, as a dependency every build reads but none rebuilds, is
# as old as the rest, and has to stay.
old="$(date -u -d '3 days ago' +%Y%m%d%H%M 2> /dev/null || date -u -v-3d +%Y%m%d%H%M)"
for file in idle/target/debug/deps/libold.rlib idle/target/debug/old/gone.o \
  active/target/debug/deps/libstable.rlib busy/target/debug/libold.rlib \
  baseline-keep/target/release/fennec quiet-turing-a1b2c3/target/debug/fennec; do
  TZ=UTC touch -t "$old" "$trees/$file"
done
hours_ago="$(date -u -d '3 hours ago' +%Y%m%d%H%M 2> /dev/null || date -u -v-3H +%Y%m%d%H%M)"
TZ=UTC touch -t "$hours_ago" "$trees/hours/target/debug/fennec"
# Something running in the busy worktree.
(cd "$trees/busy" && exec sleep 60) &
sleeper=$!
# Not this shell's job any more, so its end isn't reported.
disown "$sleeper"
sleep 1

said="$("$here/sweep-builds.sh" --worktrees "$trees" --dry-run)"
echo "$said"
grep -qx "idle: would delete 2 files, 0.0 GB" <<< "$said" || fail "the dry run didn't count the idle worktree's two files"
grep -qx "active: built in the last 2 days, so left alone" <<< "$said" || fail "the active worktree wasn't left alone"
grep -qx "busy: skipped, something is running in it" <<< "$said" || fail "the busy worktree wasn't skipped"
grep -qx "baseline-keep: kept on purpose, so left alone" <<< "$said" || fail "the -keep worktree wasn't left alone"
grep -qx "quiet-turing-a1b2c3: a session's own, so left alone" <<< "$said" || fail "the session's worktree wasn't left alone"
grep -qx "hours: built in the last 2 days, so left alone" <<< "$said" || fail "a build of three hours ago wasn't left by days"
[ -f "$trees/idle/target/debug/deps/libold.rlib" ] || fail "the dry run deleted something"

said="$("$here/sweep-builds.sh" --worktrees "$trees")"
echo "$said"
grep -qx "idle: deleted 2 files, 0.0 GB" <<< "$said" || fail "the sweep didn't say what it deleted"
[ ! -e "$trees/idle/target" ] || fail "the idle worktree's target is still there"
for swept in "$trees"/idle/target.swept.*; do
  [ ! -e "$swept" ] || fail "what was renamed out of the way is still there: $swept"
done
[ -f "$trees/active/target/debug/deps/libstable.rlib" ] || fail "an old library of an active worktree was deleted"
[ -f "$trees/active/target/debug/deps/libnew.rlib" ] || fail "a new file was deleted"
[ -f "$trees/busy/target/debug/libold.rlib" ] || fail "a file in the busy worktree was deleted"
grep -qx "freed 0.0 GB in all" <<< "$said" || fail "the sweep didn't say how much it freed"
[ -f "$trees/baseline-keep/target/release/fennec" ] || fail "the -keep worktree's build was deleted"
[ -f "$trees/quiet-turing-a1b2c3/target/debug/fennec" ] || fail "the session's build was deleted"
[ -f "$trees/hours/target/debug/fennec" ] || fail "a build of three hours ago was deleted by days"

# By hours: four leave the build of three hours ago, two take it, and the rest stay as they were.
said="$("$here/sweep-builds.sh" --worktrees "$trees" --hours 4 --dry-run)"
echo "$said"
grep -qx "hours: built in the last 4 hours, so left alone" <<< "$said" || fail "four hours took a build of three hours ago"
said="$("$here/sweep-builds.sh" --worktrees "$trees" --hours 2)"
echo "$said"
grep -qx "hours: deleted 1 files, 0.0 GB" <<< "$said" || fail "two hours didn't take a build of three hours ago"
[ ! -e "$trees/hours/target" ] || fail "the build of three hours ago is still there"
grep -qx "active: built in the last 2 hours, so left alone" <<< "$said" || fail "two hours didn't leave today's build"
[ -f "$trees/baseline-keep/target/release/fennec" ] || fail "a sweep by hours deleted the -keep worktree's build"
[ -f "$trees/quiet-turing-a1b2c3/target/debug/fennec" ] || fail "a sweep by hours deleted the session's build"
[ -f "$trees/busy/target/debug/libold.rlib" ] || fail "a sweep by hours deleted from the busy worktree"

if "$here/sweep-builds.sh" --worktrees "$trees" --days two 2> /dev/null; then
  fail "--days two was taken"
fi
for hours in two 0; do
  if "$here/sweep-builds.sh" --worktrees "$trees" --hours "$hours" 2> /dev/null; then
    fail "--hours $hours was taken"
  fi
done
if "$here/sweep-builds.sh" --worktrees "$work/none" 2> /dev/null; then
  fail "a missing folder of worktrees was taken"
fi
echo "test-sweep-builds: passed"
