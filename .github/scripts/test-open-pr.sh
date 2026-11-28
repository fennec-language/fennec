#!/usr/bin/env bash
# Tries `open-pr.sh` against a throwaway repository, with a gate and a `gh` that do as they're told,
# so that a mistake in it can't push a branch that conflicts with main or failed its gate.
set -u
# What `open-pr.sh`, and the gate and `gh` standing in for the real ones here, read from the
# environment, cleared so each case is the plain one it says unless it sets one itself: run by a
# gate that `REGATE=1 open-pr.sh` started, every case inherited `REGATE=1` and was a regate.
unset REGATE OPEN_PR EDIT_FAILS EDIT_IGNORED GATE_FAILS GATE_WRITES GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE
here="$(cd "$(dirname "$0")/../.." && pwd)"
work="$(mktemp -d)"
trap '/bin/rm -rf "$work"' EXIT
export GIT_CONFIG_GLOBAL=/dev/null GIT_AUTHOR_NAME=test GIT_AUTHOR_EMAIL=test@example.com
export GIT_COMMITTER_NAME=test GIT_COMMITTER_EMAIL=test@example.com
export GATE="$work/gate" GH="$work/gh" OPENED="$work/opened"
failures=0

check() {
  if [ "$2" = "$3" ]; then
    echo "ok: $1"
  else
    echo "FAILED: $1: expected '$2', got '$3'"
    failures=$((failures + 1))
  fi
}

# Both are written as they'll run, so what's in them expands when they do.
# shellcheck disable=SC2016
# GATE_WRITES names a file the gate writes a line to, as a test that rewrites what it expects does.
printf '#!/usr/bin/env bash\n[ -z "${GATE_FAILS:-}" ] || { echo "FAILED: tests"; exit 1; }\n[ -z "${GATE_WRITES:-}" ] || echo rewritten >> "$GATE_WRITES"\necho gate passed\n' > "$GATE"
# A `gh` that keeps one pull request's text in "$OPENED.body": `pr list` names OPEN_PR, if set, as
# the one open, `pr view` gives its text, and `pr create` and `pr edit` write it, unless EDIT_FAILS
# has the edit fail or EDIT_IGNORED has it say it worked and change nothing.
cat > "$GH" << 'GH'
#!/usr/bin/env bash
echo "$*" >> "$OPENED"
case "$1 $2" in
  "pr list") echo "${OPEN_PR:-}"; exit 0 ;;
  "pr view") cat "$OPENED.body"; exit 0 ;;
  "pr edit") [ -z "${EDIT_FAILS:-}" ] || exit 1; [ -z "${EDIT_IGNORED:-}" ] || exit 0 ;;
esac
while [ $# -gt 0 ]; do [ "$1" = --body-file ] && cat "$2" > "$OPENED.body"; shift; done
exit 0
GH
chmod +x "$GATE" "$GH"
echo "What it does." > "$work/body.md"

git init --quiet --bare --initial-branch=main "$work/origin.git"
git clone --quiet "$work/origin.git" "$work/a" 2> /dev/null
cd "$work/a" || exit 1
mkdir -p .github/scripts
cp "$here/.github/scripts/open-pr.sh" "$here/.github/scripts/fetch.sh" "$here/.github/scripts/gate-lock.sh" .github/scripts/
printf 'one\ntwo\n' > shared
git add -A
git commit --quiet -m base
git push --quiet origin HEAD:main

pushed() {
  git ls-remote origin "refs/heads/$1" | cut -f1
}

# Each branch starts from the base, before main moves on.
branch_from_base() {
  git checkout --quiet -B "$1" origin/main
}

branch_from_base conflicting
printf 'one\nours\n' > shared
git commit --quiet -am ours
branch_from_base marked
printf 'a\n<<<<<<< HEAD\nb\n=======\nc\n>>>>>>> theirs\n' > left
git add left
git commit --quiet -m "a marker left in"
branch_from_base failing
echo x > own
git add own
git commit --quiet -m "fails its gate"
branch_from_base clean
echo y > own
git add own
git commit --quiet -m "its own file"
git checkout --quiet --detach origin/main
printf 'one\nmain\n' > shared
git commit --quiet -am "main moves on"
git push --quiet origin HEAD:main

git checkout --quiet conflicting
before="$(git rev-parse HEAD)"
.github/scripts/open-pr.sh "Conflicting" "$work/body.md" > /dev/null 2> "$work/said"
check "a branch that conflicts with main stops it, and pushes nothing" "1 " "$? $(pushed conflicting)"
grep -q "merging main conflicts" "$work/said"
check "and says so" 0 "$?"
check "the merge is undone" "$before" "$(git rev-parse HEAD)"
check "with nothing left half merged" "no merge " "$(test -e .git/MERGE_HEAD && echo merging || echo no merge) $(git status --porcelain --untracked-files=no)"

git checkout --quiet marked
.github/scripts/open-pr.sh "Marked" "$work/body.md" > /dev/null 2> "$work/said"
check "a conflict marker left in a file stops it, and pushes nothing" "1 " "$? $(pushed marked)"
grep -q "conflict markers are left" "$work/said"
check "and says so" 0 "$?"

git checkout --quiet failing
GATE_FAILS=1 .github/scripts/open-pr.sh "Failing" "$work/body.md" > /dev/null 2> "$work/said"
check "a gate that fails stops it, and pushes nothing" "1 " "$? $(pushed failing)"

git checkout --quiet main 2> /dev/null || git checkout --quiet -B main origin/main
.github/scripts/open-pr.sh "On main" "$work/body.md" > /dev/null 2> "$work/said"
check "on main it stops" 1 "$?"

git checkout --quiet clean
echo uncommitted >> own
.github/scripts/open-pr.sh "Uncommitted" "$work/body.md" > /dev/null 2> "$work/said"
check "changes not committed stop it, and push nothing" "1 " "$? $(pushed clean)"
git checkout --quiet -- own

git checkout --quiet clean
echo "a module never added" > untracked.rs
.github/scripts/open-pr.sh "Untracked" "$work/body.md" > /dev/null 2> "$work/said"
check "a file not committed nor ignored stops it, and pushes nothing" "1 " "$? $(pushed clean)"
check "and names the file" yes "$(grep -q 'untracked.rs' "$work/said" && echo yes)"
/bin/rm -f untracked.rs

git checkout --quiet clean
GATE_WRITES=own .github/scripts/open-pr.sh "Rewritten" "$work/body.md" > /dev/null 2> "$work/said"
check "a gate that changes a file it tested stops it, and pushes nothing" "1 " "$? $(pushed clean)"
check "and says the gate changed what it tested" yes "$(grep -q 'the gate changed what it tested' "$work/said" && echo yes)"
git checkout --quiet -- own
git checkout --quiet clean
GATE_WRITES=made.txt .github/scripts/open-pr.sh "Made" "$work/body.md" > /dev/null 2> "$work/said"
check "a gate that leaves a file behind stops it, and pushes nothing" "1 " "$? $(pushed clean)"
/bin/rm -f made.txt

git checkout --quiet clean
# Another gate's process, running in this worktree, holds its lock.
sleep 600 &
running=$!
.github/scripts/gate-lock.sh take "$running"
.github/scripts/open-pr.sh "Beside another" "$work/body.md" > /dev/null 2> "$work/said"
check "a gate already running in the worktree stops it, and pushes nothing" "1 " "$? $(pushed clean)"
check "it names the gate's process" yes "$(grep -qF "a gate is already running in this worktree (pid $running)" "$work/said" && echo yes)"
kill "$running"
wait "$running" 2> /dev/null

check "no pull request was opened for any of those" "" "$(cat "$OPENED" 2> /dev/null)"

.github/scripts/open-pr.sh "Clean" "$work/body.md" > /dev/null 2> "$work/said"
check "a clean branch is pushed at its own commit" "0 $(git rev-parse HEAD)" "$? $(pushed clean)"
git merge-base --is-ancestor origin/main HEAD
check "with main merged in" 0 "$?"
check "and its pull request opened" "pr create --base main --head clean --title Clean --body-file" "$(sed 's/ [^ ]*$//' "$OPENED")"
# A gate other than gate.sh is a short one, and the text says so first, with the commit it gated
# and when, then what was written.
short="only \`$GATE\` ran, not \`.github/scripts/gate.sh\` and its steps. Don't merge on it."
when='[0-9]{4}-[0-9]{2}-[0-9]{2} [0-9]{2}:[0-9]{2} UTC'
# The line said for the commit checked out, with when it was gated made the same each time.
said_for_head() {
  echo "**SHORT gate** at \`$(git rev-parse --short=12 HEAD)\`, WHEN: $short"
}
first_of_text() {
  sed -n 1p "$OPENED.body" | sed -E "s/$when/WHEN/"
}
check "saying it had a SHORT gate, at this commit, then its text" "$(said_for_head)|What it does." \
  "$(first_of_text)|$(sed -n 3p "$OPENED.body")"
grep -q "SHORT gate: only $GATE ran" "$work/said"
check "and saying it where it ran" 0 "$?"

# Gated again, for a pull request already open: nothing new is opened, and the text on GitHub keeps
# everything but how it was gated.
branch_from_base regated
echo z > own
git add own
git commit --quiet -m "gated again"
: > "$OPENED"
REGATE=1 .github/scripts/open-pr.sh "Regated" "$work/body.md" > "$work/out" 2> "$work/said"
check "gated again with no pull request open, it stops before the gate and pushes nothing" "1 " "$? $(pushed regated)"
grep -q "no open pull request for regated" "$work/said"
check "and says so" 0 "$?"
grep -q "gate passed" "$work/out"
check "without running the gate" 1 "$?"

# Written as GitHub would hold it, backquotes and all.
# shellcheck disable=SC2016
printf '**Gated in full** at `0123456789ab`, 2026-01-01 00:00 UTC: every step of `gate.sh` passed: tests.\n\nWritten again by hand.\n' > "$OPENED.body"
: > "$OPENED"
OPEN_PR=7 REGATE=1 .github/scripts/open-pr.sh "Regated" "$work/body.md" > /dev/null 2> "$work/said"
check "gated again with its pull request open, it's pushed at its own commit" "0 $(git rev-parse HEAD)" "$? $(pushed regated)"
check "the old line gives way to this gate's, and the text written by hand stays" "$(said_for_head)||Written again by hand." \
  "$(first_of_text)|$(sed -n '2p;3p' "$OPENED.body" | paste -sd '|' -)"
grep -q "pr create" "$OPENED"
check "and no other pull request is opened" 1 "$?"

printf 'A text with no line saying how it was gated.\n' > "$OPENED.body"
OPEN_PR=7 REGATE=1 .github/scripts/open-pr.sh "Regated" "$work/body.md" > /dev/null 2> "$work/said"
check "a text that never said how it was gated gets the line first" "0 $(said_for_head)||A text with no line saying how it was gated." \
  "$? $(first_of_text)|$(sed -n '2p;3p' "$OPENED.body" | paste -sd '|' -)"

printf 'Unchanged.\n' > "$OPENED.body"
EDIT_FAILS=1 OPEN_PR=7 REGATE=1 .github/scripts/open-pr.sh "Regated" "$work/body.md" > /dev/null 2> "$work/said"
check "an edit that fails fails it" "1 Unchanged." "$? $(cat "$OPENED.body")"
grep -q "doesn't say it was gated again: put this first in its text by hand" "$work/said"
check "saying what to put there" 0 "$?"
EDIT_IGNORED=1 OPEN_PR=7 REGATE=1 .github/scripts/open-pr.sh "Regated" "$work/body.md" > /dev/null 2> "$work/said"
check "an edit that changes nothing fails it too" 1 "$?"
grep -q "text starts 'Unchanged.'" "$work/said"
check "saying what the text starts with" 0 "$?"

# All of it again as a gate that a regate started runs it, with `REGATE=1`, `GH=true` and an open
# pull request's number in the environment, once.
if [ -z "${TEST_OPEN_PR_AS_REGATE:-}" ]; then
  TEST_OPEN_PR_AS_REGATE=1 REGATE=1 GH=true OPEN_PR=7 "$here/.github/scripts/test-open-pr.sh" > "$work/as-regate" 2>&1
  check "and it all passes the same inside a regate's gate, with REGATE=1 and GH=true set" 0 "$?"
  grep FAILED "$work/as-regate"
fi

if [ "$failures" -gt 0 ]; then
  echo "$failures failed"
  exit 1
fi
echo "all passed"
