#!/usr/bin/env bash
# Deletes the `target/` of each worktree that nothing has built in for two days, so the worktrees
# under `.claude/worktrees` don't fill the disk. Cargo builds it all again the next time it's
# needed, so this only ever costs time.
#
# Whole folders, by when anything in them was last written, never single files by when they were
# last read: a Mac doesn't keep a file's last-read time up to date, so a library every build reads,
# built two days ago, looks unused by it, and deleting one from under a gate failed the gate
# (`-p fennec-rt --doc`, 2026-11-14). A worktree built in since is left whole.
#
# A worktree with anything running in it is left alone: a gate, cargo, or any program whose folder
# is inside it, holds a build's lock there, or names it on its command line. That's looked at again
# just before each worktree's `target/` goes, which is first renamed, in one step, so a build that
# starts a moment later finds no half-deleted folder: it builds from nothing.
#
#   sweep-builds.sh                    deletes them, and says how much it freed
#   sweep-builds.sh --dry-run          says what it would delete, deleting nothing
#   sweep-builds.sh --days N           files unused for N days rather than two
#   sweep-builds.sh --worktrees DIR    the worktrees in DIR rather than this checkout's
#
# It stops at the first thing that fails.
set -euo pipefail

dry_run=false
days=2
worktrees=""
while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run) dry_run=true ;;
    --days)
      days="${2:?--days takes a number}"
      shift
      ;;
    --worktrees)
      worktrees="${2:?--worktrees takes a folder}"
      shift
      ;;
    *)
      echo "sweep-builds: no option $1" >&2
      exit 2
      ;;
  esac
  shift
done
case "$days" in
  '' | *[!0-9]*)
    echo "sweep-builds: --days takes a whole number, not $days" >&2
    exit 2
    ;;
esac
if [ -z "$worktrees" ]; then
  # The main checkout's, wherever this runs from: the common git folder is its `.git`.
  common="$(cd "$(dirname "$0")" && git rev-parse --path-format=absolute --git-common-dir)"
  worktrees="$(dirname "$common")/.claude/worktrees"
fi
[ -d "$worktrees" ] || {
  echo "sweep-builds: there's no folder $worktrees" >&2
  exit 1
}
worktrees="$(cd "$worktrees" && pwd -P)"
minutes=$((days * 24 * 60))

folders="$(mktemp)"
commands="$(mktemp)"
listed="$(mktemp)"
trap 'rm -f "$folders" "$commands" "$listed"' EXIT

# Every running program's folder and command line, as they are now.
running() {
  lsof -a -d cwd -Fn 2> /dev/null | sed -n 's/^n//p' > "$folders" || true
  [ -s "$folders" ] || {
    echo "sweep-builds: couldn't list what's running, so it can't tell which worktrees are busy" >&2
    exit 1
  }
  ps -axww -o command= > "$commands"
}
running

# Whether anything is running in a worktree, by what was running when `running` last looked.
busy() {
  local tree="$1"
  if grep -qxF "$tree" "$folders" || grep -qF "$tree/" "$folders" || grep -qF "$tree/" "$commands"; then
    return 0
  fi
  local lock
  for lock in "$tree"/target/*/.cargo-lock "$tree"/target/*/*/.cargo-lock; do
    [ -e "$lock" ] && [ -n "$(lsof -t "$lock" 2> /dev/null)" ] && return 0
  done
  return 1
}

# How many bytes the files listed in `$1`, NUL-separated, hold.
bytes() {
  # GNU's stat, as on Linux, or BSD's, as on a Mac.
  if stat -c %s / > /dev/null 2>&1; then
    xargs -0 stat -c %s < "$1"
  else
    xargs -0 stat -f %z < "$1"
  fi | awk '{ total += $1 } END { printf "%d\n", total }'
}

shown() {
  awk -v bytes="$1" 'BEGIN { printf "%.1f GB", bytes / 1e9 }'
}

total=0
for tree in "$worktrees"/*/; do
  tree="${tree%/}"
  [ -d "$tree/target" ] || continue
  name="$(basename "$tree")"
  if busy "$tree"; then
    echo "$name: skipped, something is running in it"
    continue
  fi
  # Anything written in the time given is a build that's still in use, and all of it stays.
  if [ -n "$(find "$tree/target" -type f -mmin "-$minutes" -print -quit)" ]; then
    echo "$name: built in the last $days days, so left alone"
    continue
  fi
  find "$tree/target" -type f -print0 > "$listed"
  count="$(tr -cd '\0' < "$listed" | wc -c | tr -d ' ')"
  freed=0
  [ "$count" -eq 0 ] || freed="$(bytes "$listed")"
  if $dry_run; then
    echo "$name: would delete $count files, $(shown "$freed")"
  else
    # Looked at again, then renamed out of the way in one step, then deleted.
    running
    if busy "$tree"; then
      echo "$name: skipped, something started running in it"
      continue
    fi
    swept="$tree/target.swept.$$"
    mv "$tree/target" "$swept"
    rm -rf "$swept"
    echo "$name: deleted $count files, $(shown "$freed")"
  fi
  total=$((total + freed))
done
if $dry_run; then
  echo "would free $(shown "$total") in all"
else
  echo "freed $(shown "$total") in all"
fi
