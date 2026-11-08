#!/usr/bin/env bash
# Deletes the build files in each worktree's `target/` that nothing has read or written for two
# days, so the worktrees under `.claude/worktrees` don't fill the disk. Cargo builds again what a
# build needs and finds missing, so this only ever costs time.
#
# A worktree with anything running in it is left alone: a gate, cargo, or any program whose folder
# is inside it, holds a build's lock there, or names it on its command line.
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

# Every running program's folder, and every command line, read once.
folders="$(mktemp)"
commands="$(mktemp)"
listed="$(mktemp)"
trap 'rm -f "$folders" "$commands" "$listed"' EXIT
lsof -a -d cwd -Fn 2> /dev/null | sed -n 's/^n//p' > "$folders" || true
[ -s "$folders" ] || {
  echo "sweep-builds: couldn't list what's running, so it can't tell which worktrees are busy" >&2
  exit 1
}
ps -axww -o command= > "$commands"

# Whether anything is running in a worktree.
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
  # Neither read nor written in the time given. A file read since is one a build still uses.
  find "$tree/target" -type f -amin "+$minutes" -mmin "+$minutes" -print0 > "$listed"
  [ -s "$listed" ] || {
    echo "$name: nothing unused for $days days"
    continue
  }
  count="$(tr -cd '\0' < "$listed" | wc -c | tr -d ' ')"
  freed="$(bytes "$listed")"
  if $dry_run; then
    echo "$name: would delete $count files, $(shown "$freed")"
  else
    # A batch that fails stops the rest: xargs stops at a command that exits 255.
    xargs -0 -n 1000 sh -c 'rm -f -- "$@" || exit 255' sh < "$listed"
    find "$tree/target" -mindepth 1 -type d -empty -delete
    echo "$name: deleted $count files, $(shown "$freed")"
  fi
  total=$((total + freed))
done
if $dry_run; then
  echo "would free $(shown "$total") in all"
else
  echo "freed $(shown "$total") in all"
fi
