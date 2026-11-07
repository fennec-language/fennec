#!/usr/bin/env bash
# Opens a pull request to `main` for the branch checked out, the way `docs/landing.md` says: merges the
# latest `main` into it, runs the gate, pushes, and opens the pull request, saying the gate passed.
#
#   .github/scripts/open-pr.sh "Title" body.md
#
# It stops at the first failure, with nothing pushed and nothing opened:
#   - on `main` itself, or with changes not committed;
#   - when merging `main` conflicts: the merge is aborted, so the branch is as it was;
#   - when the merge leaves anything unmerged, or a conflict marker in a tracked file;
#   - when the gate fails;
#   - when the push fails, or leaves the branch on origin anywhere but this commit.
# Every pipe's own failure counts, so nothing after a step can hide it failing.
# GATE and GH name other commands, for its test, `test-open-pr.sh`.
set -euo pipefail
title=${1:?give a title}
body=${2:?and a file holding the text of the pull request}
gate=${GATE:-.github/scripts/gate.sh}
gh=${GH:-gh}
root=$(git rev-parse --show-toplevel)
cd "$root"
stop() {
  echo "open-pr: $*" >&2
  exit 1
}
[ -f "$body" ] || stop "there's no file $body for the pull request's text"
branch=$(git symbolic-ref --short HEAD) || stop "nothing's checked out as a branch"
[ "$branch" != main ] || stop "this is main: open a pull request from a branch of your own"
[ -z "$(git status --porcelain --untracked-files=no)" ] || stop "commit everything first"
git fetch --quiet origin
if ! git merge --quiet --no-edit origin/main; then
  echo "open-pr: merging main conflicts, in:" >&2
  git diff --name-only --diff-filter=U >&2
  git merge --abort
  stop "the merge is undone: merge main yourself, resolve it, and run this again"
fi
[ -z "$(git diff --name-only --diff-filter=U)" ] || stop "the merge left files unmerged"
if git grep --quiet -E '^(<<<<<<<|>>>>>>>)( |$)' -- . ':!*.md'; then
  git grep -n -E '^(<<<<<<<|>>>>>>>)( |$)' -- . ':!*.md' >&2
  stop "conflict markers are left in these files"
fi
"$gate" || stop "the gate failed, so nothing's pushed"
echo "What $branch changes against main:"
git diff --stat origin/main...HEAD
git push --quiet --set-upstream origin "$branch"
pushed=$(git ls-remote origin "refs/heads/$branch" | cut -f1)
[ "$pushed" = "$(git rev-parse HEAD)" ] || stop "origin's $branch is at '$pushed', not this commit"
"$gh" pr create --base main --head "$branch" --title "$title" --body-file "$body"
