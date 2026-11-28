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
#
# The pull request's text starts by saying how it was gated, so a merge is decided knowing it:
# "Gated in full", with every step `gate.sh` ran, or, when GATE names another command, "SHORT
# gate", with that command, which is no reason to merge. Either way it names the commit gated and
# when.
#
# Run again for a pull request already open, with REGATE=1 (or GH=true, as lanes have run it), it
# opens no other: it pushes, then puts the new line first in that pull request's text, in place of
# the one there, and keeps the rest as it is on GitHub, edits and all. It reads the text back and
# fails if the line isn't first. With no pull request open for the branch it stops before the gate.
# GATE and GH name other commands, for its test, `test-open-pr.sh`.
set -euo pipefail
title=${1:?give a title}
body=${2:?and a file holding the text of the pull request}
gate=${GATE:-.github/scripts/gate.sh}
gh=${GH:-gh}
regate=${REGATE:-}
# `GH=true` once kept a rerun from opening a second pull request by having it do nothing at all,
# saying nothing of the gate either. It's a rerun now, with the real `gh`.
if [ "$gh" = true ]; then
  regate=1
  gh=gh
fi
root=$(git rev-parse --show-toplevel)
cd "$root"
stop() {
  echo "open-pr: $*" >&2
  exit 1
}
# One gate a worktree (#2294): this holds the worktree's lock from before it merges main until it's
# done, and the gate it runs goes on under it.
.github/scripts/gate-lock.sh take $$ || exit 1
export FENNEC_GATE_LOCK_HOLDER=$$
trap '.github/scripts/gate-lock.sh release $$' EXIT
[ -f "$body" ] || stop "there's no file $body for the pull request's text"
branch=$(git symbolic-ref --short HEAD) || stop "nothing's checked out as a branch"
[ "$branch" != main ] || stop "this is main: open a pull request from a branch of your own"
# The gate builds and tests the working tree, and only what's committed is pushed: a file left
# out, as a module's source never added, would pass here and fail CI, so none may be.
unclean=$(git status --porcelain)
[ -z "$unclean" ] || stop "commit everything first, or add it to .gitignore: the gate tests what's here, and only commits are pushed:
$unclean"
if [ -n "$regate" ]; then
  number=$("$gh" pr list --head "$branch" --state open --json number --jq '.[0].number // empty') \
    || stop "couldn't ask GitHub for $branch's pull request"
  [ -n "$number" ] || stop "there's no open pull request for $branch to gate again: run it without REGATE or GH=true to open one"
fi
# Under the lock every worktree shares, as two gates fetching at once raced on one ref.
.github/scripts/fetch.sh
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
said=$(mktemp)
trap 'rm -f "$said" "$said.md" "$said.was"; .github/scripts/gate-lock.sh release $$' EXIT
gated_head=$(git rev-parse HEAD)
"$gate" | tee "$said" || stop "the gate failed, so nothing's pushed"
# What passed has to be what's pushed: a gate that changed a file, as a test rewriting what it
# expects does, or committed, tested something else.
unclean=$(git status --porcelain)
[ -z "$unclean" ] || stop "the gate changed what it tested, so nothing's pushed:
$unclean"
[ "$(git rev-parse HEAD)" = "$gated_head" ] || stop "the gate moved $branch from $gated_head, so nothing's pushed"
gated="at \`$(git rev-parse --short=12 HEAD)\`, $(date -u '+%Y-%m-%d %H:%M UTC')"
if [ "$gate" = .github/scripts/gate.sh ]; then
  # Every step it ran, as its summary lists them: it passes only when each one did, or failed only
  # on what main is known to fail, which is said.
  steps=$(sed -n '/^the gate.s steps:$/,/altogether$/p' "$said" \
    | awk '/^  [a-z]/ { status = $1; sub(/^  [a-z]+ +/, ""); print (status == "known" ? $0 " (past main'"'"'s known failures)" : $0) }' \
    | paste -sd ',' - | sed 's/,/, /g')
  how="**Gated in full** $gated: every step of \`gate.sh\` passed: ${steps:-its summary was not found}."
else
  how="**SHORT gate** $gated: only \`$gate\` ran, not \`.github/scripts/gate.sh\` and its steps. Don't merge on it."
  echo "open-pr: SHORT gate: only $gate ran" >&2
fi
echo "What $branch changes against main:"
git diff --stat origin/main...HEAD
git push --quiet --set-upstream origin "$branch"
pushed=$(git ls-remote origin "refs/heads/$branch" | cut -f1)
[ "$pushed" = "$(git rev-parse HEAD)" ] || stop "origin's $branch is at '$pushed', not this commit"
if [ -z "$regate" ]; then
  { echo "$how"; echo; cat "$body"; } > "$said.md"
  "$gh" pr create --base main --head "$branch" --title "$title" --body-file "$said.md"
  exit 0
fi
# Gated again: the text on GitHub keeps all but its first line, when that says how it was gated.
unsaid="it's pushed, but pull request #$number doesn't say it was gated again: put this first in its text by hand: $how"
"$gh" pr view "$number" --json body --jq .body > "$said.was" || stop "couldn't read pull request #$number's text: $unsaid"
# Read without a pipe, whose early end under pipefail would read as a line that doesn't match.
was_first=''
IFS= read -r was_first < "$said.was" || true
case "$was_first" in
  '**Gated in full**'* | '**SHORT gate**'*) { echo "$how"; tail -n +2 "$said.was"; } > "$said.md" ;;
  *) { echo "$how"; echo; cat "$said.was"; } > "$said.md" ;;
esac
"$gh" pr edit "$number" --body-file "$said.md" > /dev/null || stop "couldn't change pull request #$number's text: $unsaid"
"$gh" pr view "$number" --json body --jq .body > "$said.was" || stop "couldn't read pull request #$number's text back: $unsaid"
first=''
IFS= read -r first < "$said.was" || true
[ "$first" = "$how" ] || stop "pull request #$number's text starts '$first': $unsaid"
echo "open-pr: pull request #$number now starts: $how"
