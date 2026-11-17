#!/usr/bin/env bash
# Tests sweep-worktrees.sh on a repository of its own, with a worktree for each case: it removes
# the landed ones, by their commit on main or by their merged pull request, and keeps every other
# one, and the branches of those it removes.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
sleeper=""
trap '[ -z "$sleeper" ] || { kill "$sleeper" && wait "$sleeper"; } 2> /dev/null || true; rm -rf "$work"' EXIT
export GIT_AUTHOR_NAME=test GIT_AUTHOR_EMAIL=test@example.com GIT_COMMITTER_NAME=test GIT_COMMITTER_EMAIL=test@example.com
old="2026-01-01T00:00:00"
git init --quiet --bare --initial-branch=main "$work/origin.git"
git clone --quiet "$work/origin.git" "$work/checkout" 2> /dev/null
cd "$work/checkout"
echo one > file
git add file
GIT_COMMITTER_DATE="$old" git commit --quiet -m one
git push --quiet origin HEAD:main
mkdir -p .claude/worktrees

# A worktree on a branch of its own with one commit, made long ago.
made() {
  git worktree add --quiet -b "$2" ".claude/worktrees/$1" main
  echo "$1" > ".claude/worktrees/$1/$1"
  git -C ".claude/worktrees/$1" add "$1"
  GIT_COMMITTER_DATE="$old" git -C ".claude/worktrees/$1" commit --quiet -m "$1"
}
# Its commit put on main, as a merge that keeps it.
landed() {
  git merge --quiet --no-edit "$1"
  git push --quiet origin HEAD:main
}
made landed h-landed
landed h-landed
made dirty h-dirty
landed h-dirty
made base-keep h-keep
landed h-keep
made unlanded h-unlanded
made squashed h-squashed
made brave-curie-1a2b3c h-session
landed h-session
made busy h-busy
landed h-busy
made recent h-recent
landed h-recent
echo changed >> .claude/worktrees/dirty/dirty
# Everything as old as its commits, but for `recent`, which was used a moment ago.
find .claude/worktrees -exec touch -t 202601010000 {} +
touch .claude/worktrees/recent/recent
(cd .claude/worktrees/busy && exec sleep 300) &
sleeper=$!
# A `gh` that knows one merged pull request, a squash merge's, for h-squashed.
cat > "$work/gh" << 'EOF'
#!/bin/sh
case "$*" in *"--head h-squashed"*) echo 1901 ;; esac
EOF
chmod +x "$work/gh"
said="$(GH="$work/gh" "$here/sweep-worktrees.sh")"
echo "$said"

failed=0
expect() {
  if ! grep -qxF "$1" <<< "$said"; then
    echo "not ok: expected \"$1\"" >&2
    failed=1
  fi
}
expect "landed: removed, its branch kept"
expect "squashed: removed, its branch kept"
expect "dirty: stays, it has uncommitted changes"
expect "base-keep: stays, kept on purpose"
expect "unlanded: stays, h-unlanded hasn't landed"
expect "brave-curie-1a2b3c: stays, a session's own"
expect "busy: stays, something is running in it"
expect "recent: stays, it was used in the last 6 hours"
expect "removed 2 worktrees"
for gone in landed squashed; do
  [ ! -e ".claude/worktrees/$gone" ] || { echo "not ok: $gone is still there" >&2; failed=1; }
done
for kept in h-landed h-squashed; do
  git rev-parse --quiet --verify "refs/heads/$kept" > /dev/null || { echo "not ok: branch $kept is gone" >&2; failed=1; }
done
[ "$failed" -eq 0 ] && echo "ok: sweep-worktrees removes only landed, idle, clean worktrees, and keeps their branches"
exit "$failed"
