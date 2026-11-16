#!/usr/bin/env bash
# Removes the worktrees under `.claude/worktrees` that are done with: their branch has landed on
# main, nothing in them has changed for six hours, nothing is running in them, and git sees nothing
# in them uncommitted. Each goes with `git worktree remove`, never forced, so git itself refuses one
# with changes; its branch is kept, so nothing is lost. Run weekly, or whenever the disk fills.
#
# Landed means its branch's pull request was merged, which a squash merge leaves no other trace of,
# or its last commit is on main. Left alone whatever they hold:
#   - one whose name ends in `-keep`, `-base` or `-gate`: a lane's baseline or gate worktree,
#     which it keeps on purpose;
#   - one the app made for a session, named like `blissful-lederberg-d82a7e`: that session works
#     in it;
#   - one with no branch checked out, which only a lane knows the use of.
#
# Something running in a worktree is a program whose folder is inside it, or that names it on its
# command line. What's running is listed into files first and searched there, so the search never
# finds itself.
#
#   sweep-worktrees.sh                 removes them, saying which and why each other one stays
#   sweep-worktrees.sh --dry-run       says what it would remove, removing nothing
#   sweep-worktrees.sh --hours N       idle for N hours rather than six
#
# GH names the `gh` it asks about pull requests, for its test, `test-sweep-worktrees.sh`.
set -euo pipefail

dry_run=false
hours=6
while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run) dry_run=true ;;
    --hours)
      hours="${2:?--hours takes a number}"
      shift
      ;;
    *)
      echo "sweep-worktrees: no option $1" >&2
      exit 2
      ;;
  esac
  shift
done
case "$hours" in
  '' | *[!0-9]*)
    echo "sweep-worktrees: --hours takes a whole number, not $hours" >&2
    exit 2
    ;;
esac
gh=${GH:-gh}
# The main checkout's, wherever this runs from: the common git folder is its `.git`.
common="$(git rev-parse --path-format=absolute --git-common-dir)"
checkout="$(dirname "$common")"
worktrees="$checkout/.claude/worktrees"
[ -d "$worktrees" ] || {
  echo "sweep-worktrees: there's no folder $worktrees" >&2
  exit 1
}
worktrees="$(cd "$worktrees" && pwd -P)"
# From the checkout, so `gh` asks about this repository's pull requests.
cd "$checkout"
git fetch --quiet origin main

folders="$(mktemp)"
commands="$(mktemp)"
since="$(mktemp)"
trap 'rm -f "$folders" "$commands" "$since"' EXIT
# A file as old as the idle time, to find anything newer than it.
touch -t "$(date -v -"${hours}"H '+%Y%m%d%H%M.%S' 2> /dev/null || date -d "-$hours hours" '+%Y%m%d%H%M.%S')" "$since"

# Every running program's folder and command line, as they are now.
running() {
  lsof -a -d cwd -Fn 2> /dev/null | sed -n 's/^n//p' > "$folders" || true
  [ -s "$folders" ] || {
    echo "sweep-worktrees: couldn't list what's running, so it can't tell which worktrees are busy" >&2
    exit 1
  }
  ps -axww -o command= > "$commands"
}

# Whether anything is running in a worktree, by what `running` last listed.
busy() {
  grep -qxF "$1" "$folders" || grep -qF "$1/" "$folders" || grep -qF "$1/" "$commands"
}

# Why a worktree stays, or nothing when it can go.
stays() {
  local tree="$1" name branch
  name="$(basename "$tree")"
  case "$name" in
    *-keep | *-base | *-gate) echo "kept on purpose" && return ;;
  esac
  if [[ "$name" =~ ^[a-z]+-[a-z]+-[0-9a-f]{6}$ ]]; then
    echo "a session's own" && return
  fi
  branch="$(git -C "$tree" symbolic-ref --quiet --short HEAD)" || {
    echo "no branch checked out" && return
  }
  [ -z "$(git -C "$tree" status --porcelain)" ] || {
    echo "it has uncommitted changes" && return
  }
  if ! git -C "$tree" merge-base --is-ancestor HEAD origin/main \
    && [ -z "$("$gh" pr list --head "$branch" --state merged --json number --jq '.[].number' 2> /dev/null)" ]; then
    echo "$branch hasn't landed" && return
  fi
  # Its last commit, its own files and the top of its build, any of them newer than the idle time.
  if [ "$(git -C "$tree" log -1 --format=%ct)" -gt "$(date -r "$since" +%s)" ] \
    || [ -n "$(find "$tree" -path "$tree/target" -prune -o -newer "$since" -print -quit)" ] \
    || [ -n "$(find "$tree/target" -maxdepth 2 -newer "$since" -print -quit 2> /dev/null)" ]; then
    echo "it was used in the last $hours hours" && return
  fi
  running
  busy "$tree" && echo "something is running in it" && return
  return 0
}

removed=0
for tree in "$worktrees"/*/; do
  tree="${tree%/}"
  [ -e "$tree/.git" ] || continue
  why="$(stays "$tree")"
  if [ -n "$why" ]; then
    echo "$(basename "$tree"): stays, $why"
  elif $dry_run; then
    echo "$(basename "$tree"): would go"
    removed=$((removed + 1))
  else
    git -C "$checkout" worktree remove "$tree"
    echo "$(basename "$tree"): removed, its branch kept"
    removed=$((removed + 1))
  fi
done
if $dry_run; then
  echo "would remove $removed worktrees"
else
  git -C "$checkout" worktree prune
  echo "removed $removed worktrees"
fi
