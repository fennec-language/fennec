#!/usr/bin/env bash
# Tests gate-lock.sh on a repository of its own with a second worktree. Its lock holds a worktree to
# one gate: a second one refuses at once, naming the first's process. A lock whose process is gone
# is taken over, only its holder lets it go, and another worktree's gate doesn't wait on it.
set -u
here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
sleeping=()
cleanup() {
  local each
  for each in "${sleeping[@]}"; do
    kill "$each" 2> /dev/null
    wait "$each" 2> /dev/null
  done
  /bin/rm -rf "$work"
}
trap cleanup EXIT
failures=0

check() {
  if [ "$2" = "$3" ]; then
    echo "ok: $1"
  else
    echo "FAILED: $1: expected '$2', got '$3'"
    failures=$((failures + 1))
  fi
}

export GIT_AUTHOR_NAME=test GIT_AUTHOR_EMAIL=test@example.com GIT_COMMITTER_NAME=test GIT_COMMITTER_EMAIL=test@example.com
git init --quiet --initial-branch=main "$work/repo"
cd "$work/repo" || exit 1
echo one > file
git add file
git commit --quiet -m one
git worktree add --quiet -b other "$work/other" main

# A process standing in for a gate, alive until the test ends, as `$last`.
gate() {
  sleep 600 > /dev/null 2>&1 &
  last=$!
  sleeping+=("$last")
}
lock() {
  "$here/gate-lock.sh" "$@" 2> "$work/said"
  echo $?
}
said() {
  if grep -qF -- "$1" "$work/said"; then echo yes; else echo no; fi
}

gate
first=$last
gate
second=$last
check "a gate takes the worktree's lock" 0 "$(lock take "$first")"
check "and takes it again, as its own" 0 "$(lock take "$first")"
check "a second gate in the worktree is refused" 1 "$(lock take "$second")"
check "naming the first gate's process" yes "$(said "a gate is already running in this worktree (pid $first)")"
check "a gate in another worktree takes its own" 0 "$(cd "$work/other" && lock take "$second")"
check "the second gate can't let the first's go" 0 "$(lock release "$second")"
check "so it's refused still" 1 "$(lock take "$second")"
check "the first lets it go" 0 "$(lock release "$first")"
check "and the second takes it" 0 "$(lock take "$second")"

gate
gone=$last
kill "$second"
wait "$second" 2> /dev/null
check "a lock whose gate is gone is taken over" 0 "$(lock take "$gone")"
check "by the gate that took it" 1 "$(lock take "$first")"
check "which it names" yes "$(said "(pid $gone)")"

if [ "$failures" -ne 0 ]; then
  echo "$failures failed"
  exit 1
fi
echo "every check passed"
